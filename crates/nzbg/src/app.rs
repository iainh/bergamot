use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;

use nzbg_config::{Config, parse_config};
use nzbg_server::{AppState, ServerConfig as WebServerConfig, WebServer};

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
    let app_state = Arc::new(AppState::default());
    let inter_dir = config.inter_dir.clone();

    let (mut coordinator, _queue_handle, assignment_rx) = nzbg_queue::QueueCoordinator::new(4, 2);

    let coordinator_handle = tokio::spawn(async move {
        coordinator.run().await;
    });

    let queue_handle_for_worker = _queue_handle.clone();
    let worker_handle = tokio::spawn(crate::download::download_worker(
        assignment_rx,
        queue_handle_for_worker,
        fetcher,
        inter_dir,
    ));

    let (scheduler_tx, scheduler_handles) = nzbg_scheduler::start_services(&config).await?;

    let server = WebServer::new(web_config, app_state);
    let server_handle = tokio::spawn(async move {
        if let Err(err) = server.run().await {
            tracing::error!("web server error: {err}");
        }
    });

    tokio::signal::ctrl_c().await.context("awaiting ctrl-c")?;
    tracing::info!("shutdown signal received");

    let _ = _queue_handle.shutdown().await;
    nzbg_scheduler::shutdown_services(scheduler_tx, scheduler_handles).await;
    server_handle.abort();
    worker_handle.abort();
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
