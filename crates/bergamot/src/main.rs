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
    config: &bergamot_config::Config,
    stats: Option<Arc<dyn bergamot_nntp::StatsRecorder>>,
) -> Arc<dyn download::ArticleFetcher> {
    let servers: Vec<bergamot_nntp::NewsServer> = config
        .servers
        .iter()
        .map(|s| bergamot_nntp::NewsServer {
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
                true => bergamot_nntp::Encryption::Tls,
                false => bergamot_nntp::Encryption::None,
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
                bergamot_config::IpVersion::Auto => bergamot_nntp::IpVersion::Auto,
                bergamot_config::IpVersion::IPv4 => bergamot_nntp::IpVersion::IPv4Only,
                bergamot_config::IpVersion::IPv6 => bergamot_nntp::IpVersion::IPv6Only,
            },
            cert_verification: s.cert_verification,
        })
        .collect();

    let mut pool = bergamot_nntp::ServerPool::new(servers);
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
        .thread_name_fn(|| {
            static IDX: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let i = IDX.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            format!("bergamot-{i}")
        })
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

        tracing::info!("bergamot starting");
        let stats_tracker = bergamot_scheduler::StatsTracker::from_config(&config);
        let shared_stats =
            std::sync::Arc::new(bergamot_scheduler::SharedStatsTracker::new(stats_tracker));
        let fetcher = build_fetcher(
            &config,
            Some(shared_stats.clone() as Arc<dyn bergamot_nntp::StatsRecorder>),
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
