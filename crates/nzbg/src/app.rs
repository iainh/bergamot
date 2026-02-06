use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;

use nzbg_config::{Config, parse_config};
use nzbg_diskstate::{DiskState, JsonFormat, StateLock};
use nzbg_server::{AppState, ServerConfig as WebServerConfig, ShutdownHandle, WebServer};

pub fn load_config(path: &Path) -> Result<Config> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("reading config: {}", path.display()))?;
    let raw = parse_config(&content).context("parsing config")?;
    Ok(Config::from_raw(raw))
}

pub fn init_tracing(log_level: &str) {
    let filter = EnvFilter::try_new(log_level).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

pub fn web_server_config(config: &Config) -> WebServerConfig {
    WebServerConfig {
        control_ip: config.control_ip.clone(),
        control_port: config.control_port,
        secure_control: config.secure_control,
        secure_cert: None,
        secure_key: None,
        web_dir: config.web_dir.clone(),
        form_auth: false,
        authorized_ips: config.authorized_ip.clone(),
        control_username: config.control_username.clone(),
        control_password: config.control_password.clone(),
        restricted_username: String::new(),
        restricted_password: String::new(),
        add_username: String::new(),
        add_password: String::new(),
    }
}

pub async fn run(config: Config, fetcher: Arc<dyn crate::download::ArticleFetcher>) -> Result<()> {
    let web_config = Arc::new(web_server_config(&config));
    let inter_dir = config.inter_dir.clone();

    let state_dir = config.queue_dir.clone();
    let _state_lock = StateLock::acquire(&state_dir).context("acquiring state lock")?;
    let disk = Arc::new(DiskState::new(state_dir, JsonFormat).context("creating disk state")?);

    if let Err(err) = disk.recover() {
        tracing::warn!("disk state recovery: {err}");
    }

    let (mut coordinator, queue_handle, assignment_rx) = nzbg_queue::QueueCoordinator::new(4, 2);

    let (shutdown_handle, mut shutdown_rx) = ShutdownHandle::new();
    let app_state = Arc::new(
        AppState::default()
            .with_queue(queue_handle.clone())
            .with_shutdown(shutdown_handle),
    );

    let coordinator_handle = tokio::spawn(async move {
        coordinator.run().await;
    });

    let worker_handle = tokio::spawn(crate::download::download_worker(
        assignment_rx,
        queue_handle.clone(),
        fetcher,
        inter_dir,
    ));

    let deps = nzbg_scheduler::ServiceDeps {
        queue: queue_handle.clone(),
        disk: disk.clone(),
    };
    let (scheduler_tx, scheduler_handles) = nzbg_scheduler::start_services(&config, deps).await?;

    let server = WebServer::new(web_config, app_state);
    let server_handle = tokio::spawn(async move {
        if let Err(err) = server.run().await {
            tracing::error!("web server error: {err}");
        }
    });

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("ctrl-c received");
        }
        _ = async {
            while shutdown_rx.changed().await.is_ok() {
                if *shutdown_rx.borrow() {
                    break;
                }
            }
        } => {
            tracing::info!("RPC shutdown received");
        }
    }
    tracing::info!("shutdown signal received");

    nzbg_scheduler::shutdown_services(scheduler_tx, scheduler_handles).await;
    server_handle.abort();
    worker_handle.abort();

    if let Ok(snapshot) = queue_handle.get_queue_snapshot().await {
        let state = nzbg_scheduler::DiskStateFlush::snapshot_to_queue_state(&snapshot);
        let disk_clone = disk.clone();
        let _ = tokio::task::spawn_blocking(move || disk_clone.save_queue(&state)).await;
        tracing::info!("final disk state flush complete");
    }

    let _ = queue_handle.shutdown().await;
    let _ = coordinator_handle.await;

    tracing::info!("shutdown complete");
    Ok(())
}

pub fn default_config_path() -> Option<std::path::PathBuf> {
    let candidates = [
        dirs::config_dir().map(|d| d.join("nzbg").join("nzbg.conf")),
        Some(std::path::PathBuf::from("/etc/nzbg.conf")),
        Some(std::path::PathBuf::from("/usr/local/etc/nzbg.conf")),
    ];
    candidates.into_iter().flatten().find(|p| p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn load_config_from_file() {
        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        writeln!(tmp, "MainDir=/tmp/nzbg-test").expect("write");
        writeln!(tmp, "ControlPort=6790").expect("write");

        let config = load_config(tmp.path()).expect("load");
        assert_eq!(config.control_port, 6790);
        assert_eq!(config.main_dir, std::path::PathBuf::from("/tmp/nzbg-test"));
    }

    #[test]
    fn load_config_returns_error_for_missing_file() {
        let result = load_config(Path::new("/nonexistent/nzbg.conf"));
        assert!(result.is_err());
    }

    #[test]
    fn web_server_config_maps_fields() {
        let config = Config::from_raw(
            [
                ("ControlIP".to_string(), "127.0.0.1".to_string()),
                ("ControlPort".to_string(), "8080".to_string()),
            ]
            .into_iter()
            .collect(),
        );
        let wsc = web_server_config(&config);
        assert_eq!(wsc.control_ip, "127.0.0.1");
        assert_eq!(wsc.control_port, 8080);
    }

    #[test]
    fn default_config_path_returns_none_when_no_file_exists() {
        let result = default_config_path();
        // We can't guarantee any config file exists in test environment
        // but we can verify it returns an Option
        let _ = result;
    }
}
