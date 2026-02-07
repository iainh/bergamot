mod app;
mod cache;
mod cli;
mod daemon;
mod download;
mod writer;

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;

use crate::cli::Cli;

fn build_fetcher(
    config: &nzbg_config::Config,
    stats: Option<Arc<dyn nzbg_nntp::StatsRecorder>>,
) -> Arc<dyn download::ArticleFetcher> {
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

    let mut pool = nzbg_nntp::ServerPool::new(servers);
    if let Some(stats) = stats {
        pool = pool.with_stats(stats);
    }
    Arc::new(download::NntpPoolFetcher::new(pool))
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if !cli.foreground {
        daemon::daemonize()?;
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;

    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    rt.block_on(async {
        let _pidfile = cli
            .pidfile
            .as_deref()
            .map(daemon::PidFile::create)
            .transpose()?;

        let config_path = match cli.config {
            Some(path) => path,
            None => app::default_config_path()
                .context("no config file found; use --config to specify one")?,
        };

        let config = app::load_config(&config_path)?;
        let log_buffer = app::init_tracing(&cli.log_level);

        tracing::info!("nzbg starting");
        let stats_tracker = nzbg_scheduler::StatsTracker::from_config(&config);
        let shared_stats =
            std::sync::Arc::new(nzbg_scheduler::SharedStatsTracker::new(stats_tracker));
        let fetcher = build_fetcher(
            &config,
            Some(shared_stats.clone() as Arc<dyn nzbg_nntp::StatsRecorder>),
        );
        app::run_with_config_path(
            config,
            fetcher,
            Some(config_path),
            Some(log_buffer),
            Some(shared_stats.clone()),
        )
        .await
    })
}
