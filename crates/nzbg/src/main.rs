mod app;
mod cli;
mod download;

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;

use crate::cli::Cli;

fn build_fetcher(config: &nzbg_config::Config) -> Arc<dyn download::ArticleFetcher> {
    let servers: Vec<nzbg_nntp::NewsServer> = config
        .servers
        .iter()
        .map(|s| nzbg_nntp::NewsServer {
            id: s.id,
            name: s.name.clone(),
            active: s.active,
            host: s.host.clone(),
            port: s.port,
            username: if s.username.is_empty() {
                None
            } else {
                Some(s.username.clone())
            },
            password: if s.password.is_empty() {
                None
            } else {
                Some(s.password.clone())
            },
            encryption: match s.encryption {
                true => nzbg_nntp::Encryption::Tls,
                false => nzbg_nntp::Encryption::None,
            },
            cipher: if s.cipher.is_empty() {
                None
            } else {
                Some(s.cipher.clone())
            },
            connections: s.connections,
            retention: s.retention,
            level: s.level,
            optional: s.optional,
            group: s.group,
            join_group: true,
            ip_version: match s.ip_version {
                nzbg_config::IpVersion::Auto => nzbg_nntp::IpVersion::Auto,
                nzbg_config::IpVersion::IPv4 => nzbg_nntp::IpVersion::IPv4Only,
                nzbg_config::IpVersion::IPv6 => nzbg_nntp::IpVersion::IPv6Only,
            },
            cert_verification: s.cert_verification,
        })
        .collect();

    let pool = nzbg_nntp::ServerPool::new(servers);
    Arc::new(download::NntpPoolFetcher::new(pool))
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
    let _log_buffer = app::init_tracing(&cli.log_level);

    tracing::info!("nzbg starting");
    let fetcher = build_fetcher(&config);
    app::run(config, fetcher).await
}
