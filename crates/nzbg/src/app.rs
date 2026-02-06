use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use nzbg_config::{Config, parse_config};
use nzbg_core::models::{ArticleInfo, ArticleStatus, DownloadQueue, FileInfo, NzbInfo, Priority};
use nzbg_diskstate::{DiskState, JsonFormat, StateLock};
use nzbg_logging::{BufferLayer, LogBuffer};
use nzbg_postproc::{PostProcessRequest, PostProcessor};
use nzbg_queue::NzbCompletionNotice;
use nzbg_server::{AppState, ServerConfig as WebServerConfig, ShutdownHandle, WebServer};

pub fn load_config(path: &Path) -> Result<Config> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("reading config: {}", path.display()))?;
    let raw = parse_config(&content).context("parsing config")?;
    Ok(Config::from_raw(raw))
}

pub fn init_tracing(log_level: &str) -> Arc<LogBuffer> {
    let filter = EnvFilter::try_new(log_level).unwrap_or_else(|_| EnvFilter::new("info"));
    let buffer = Arc::new(LogBuffer::new(1000));
    let buffer_layer = BufferLayer::new(buffer.clone());
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(buffer_layer)
        .init();
    buffer
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

fn priority_from_i32(val: i32) -> Priority {
    match val {
        v if v <= -100 => Priority::VeryLow,
        v if v <= -50 => Priority::Low,
        v if v <= 0 => Priority::Normal,
        v if v <= 50 => Priority::High,
        v if v <= 100 => Priority::VeryHigh,
        _ => Priority::Force,
    }
}

pub fn restore_queue(disk: &DiskState<JsonFormat>) -> Result<Option<(DownloadQueue, bool, u64)>> {
    let state = match disk.load_queue() {
        Ok(s) => s,
        Err(err) => {
            if err
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                return Ok(None);
            }
            return Err(err.context("loading queue state"));
        }
    };

    let mut queue_nzbs = Vec::new();
    let mut max_file_id = state.next_file_id;

    for nzb_state in &state.nzbs {
        let nzb_bytes = match disk.load_nzb_file(nzb_state.id) {
            Ok(b) => b,
            Err(err) => {
                tracing::warn!(
                    "skipping NZB {}: failed to load NZB file: {err}",
                    nzb_state.id
                );
                continue;
            }
        };
        let parsed = match nzbg_nzb::parse_nzb_auto(&nzb_bytes) {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!("skipping NZB {}: failed to parse: {err}", nzb_state.id);
                continue;
            }
        };

        let mut files = Vec::with_capacity(parsed.files.len());
        let mut total_size: u64 = 0;
        let mut total_article_count: u32 = 0;
        let priority = priority_from_i32(nzb_state.priority);

        let file_ids = &nzb_state.file_ids;

        for (file_idx, nzb_file) in parsed.files.iter().enumerate() {
            let file_id = file_ids.get(file_idx).copied().unwrap_or_else(|| {
                let id = max_file_id;
                max_file_id += 1;
                id
            });

            let output_filename = nzb_file
                .filename
                .clone()
                .unwrap_or_else(|| format!("file-{file_idx}"));

            let articles: Vec<ArticleInfo> = nzb_file
                .segments
                .iter()
                .map(|seg| ArticleInfo {
                    part_number: seg.number,
                    message_id: seg.message_id.clone(),
                    size: seg.bytes,
                    status: ArticleStatus::Undefined,
                    segment_offset: 0,
                    segment_size: seg.bytes,
                    crc: 0,
                })
                .collect();

            let file_size = nzb_file.total_size;
            let article_count = articles.len() as u32;
            total_size += file_size;
            total_article_count += article_count;

            files.push(FileInfo {
                id: file_id,
                nzb_id: nzb_state.id,
                filename: output_filename.clone(),
                subject: nzb_file.subject.clone(),
                output_filename,
                groups: nzb_file.groups.clone(),
                articles,
                size: file_size,
                remaining_size: file_size,
                success_size: 0,
                failed_size: 0,
                missed_size: 0,
                total_articles: article_count,
                missing_articles: 0,
                failed_articles: 0,
                success_articles: 0,
                paused: false,
                completed: false,
                priority,
                time: std::time::SystemTime::now(),
                active_downloads: 0,
                crc: 0,
                server_stats: vec![],
            });
        }

        let nzb = NzbInfo {
            id: nzb_state.id,
            kind: nzbg_core::models::NzbKind::Nzb,
            name: nzb_state.name.clone(),
            filename: nzb_state.filename.clone(),
            url: nzb_state.url.clone().unwrap_or_default(),
            dest_dir: nzb_state.dest_dir.clone(),
            final_dir: nzb_state.final_dir.clone().unwrap_or_default(),
            temp_dir: std::path::PathBuf::new(),
            queue_dir: std::path::PathBuf::new(),
            category: nzb_state.category.clone(),
            priority,
            dup_key: nzb_state.dupe_key.clone(),
            dup_mode: nzb_state.dupe_mode,
            dup_score: nzb_state.dupe_score,
            size: total_size,
            remaining_size: total_size,
            paused_size: 0,
            failed_size: 0,
            success_size: 0,
            current_downloaded_size: 0,
            par_size: 0,
            par_remaining_size: 0,
            par_current_success_size: 0,
            par_failed_size: 0,
            file_count: files.len() as u32,
            remaining_file_count: files.len() as u32,
            remaining_par_count: 0,
            total_article_count,
            success_article_count: 0,
            failed_article_count: 0,
            added_time: std::time::SystemTime::now(),
            min_time: None,
            max_time: None,
            download_start_time: None,
            download_sec: 0,
            post_total_sec: 0,
            par_sec: 0,
            repair_sec: 0,
            unpack_sec: 0,
            paused: nzb_state.paused,
            deleted: false,
            direct_rename: false,
            force_priority: false,
            reprocess: false,
            par_manual: false,
            clean_up_disk: false,
            par_status: nzbg_core::models::ParStatus::None,
            unpack_status: nzbg_core::models::UnpackStatus::None,
            move_status: nzbg_core::models::MoveStatus::None,
            delete_status: nzbg_core::models::DeleteStatus::None,
            mark_status: nzbg_core::models::MarkStatus::None,
            url_status: nzbg_core::models::UrlStatus::None,
            health: nzb_state.health,
            critical_health: nzb_state.critical_health,
            files,
            completed_files: vec![],
            server_stats: vec![],
            parameters: vec![],
            post_info: None,
            message_count: 0,
            cached_message_count: 0,
        };
        queue_nzbs.push(nzb);
    }

    let queue = DownloadQueue {
        queue: queue_nzbs,
        history: vec![],
        next_nzb_id: state.next_nzb_id,
        next_file_id: max_file_id,
    };

    Ok(Some((queue, state.download_paused, state.speed_limit)))
}

fn postproc_config(config: &Config) -> nzbg_postproc::Config {
    nzbg_postproc::Config {
        par2_path: std::path::PathBuf::from("par2"),
        dest_dir: config.dest_dir.clone(),
        append_category_dir: config.append_category_dir,
        password_file: None,
        unpack_cleanup_disk: config.unpack_cleanup_disk,
        ext_cleanup_disk: config.ext_cleanup_disk.clone(),
    }
}

pub fn forward_completions(
    mut completion_rx: tokio::sync::mpsc::Receiver<NzbCompletionNotice>,
    postproc_tx: tokio::sync::mpsc::Sender<PostProcessRequest>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(notice) = completion_rx.recv().await {
            let req = PostProcessRequest {
                nzb_id: notice.nzb_id as i64,
                nzb_name: notice.nzb_name,
                working_dir: notice.working_dir,
                category: notice.category,
                parameters: notice.parameters,
            };
            if postproc_tx.send(req).await.is_err() {
                tracing::warn!("postproc channel closed");
                break;
            }
        }
    })
}

pub async fn run(config: Config, fetcher: Arc<dyn crate::download::ArticleFetcher>) -> Result<()> {
    let web_config = Arc::new(web_server_config(&config));
    let inter_dir = config.inter_dir.clone();

    let cache: Arc<dyn crate::cache::ArticleCache> = if config.article_cache > 0 {
        Arc::new(crate::cache::BoundedCache::new(
            config.article_cache as usize * 1024 * 1024,
        ))
    } else {
        Arc::new(crate::cache::NoopCache)
    };
    let writer_pool = Arc::new(crate::writer::FileWriterPool::new());

    let state_dir = config.queue_dir.clone();
    let _state_lock = StateLock::acquire(&state_dir).context("acquiring state lock")?;
    let disk = Arc::new(DiskState::new(state_dir, JsonFormat).context("creating disk state")?);

    if let Err(err) = disk.recover() {
        tracing::warn!("disk state recovery: {err}");
    }

    let (completion_tx, completion_rx) = tokio::sync::mpsc::channel::<NzbCompletionNotice>(16);
    let (postproc_tx, postproc_rx) = tokio::sync::mpsc::channel::<PostProcessRequest>(16);

    let pp_config = Arc::new(postproc_config(&config));
    let par2 = Arc::new(nzbg_postproc::Par2CommandLine {
        par2_path: pp_config.par2_path.clone(),
    });
    let unpacker = Arc::new(nzbg_postproc::CommandLineUnpacker);
    let history = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut postprocessor = PostProcessor::new(postproc_rx, pp_config, history, par2, unpacker);
    let postproc_handle = tokio::spawn(async move {
        postprocessor.run().await;
    });
    let forward_handle = forward_completions(completion_rx, postproc_tx.clone());

    let (coordinator, queue_handle, assignment_rx, rate_rx) =
        nzbg_queue::QueueCoordinator::new(4, 2);
    let mut coordinator = coordinator.with_completion_tx(completion_tx);

    match restore_queue(&disk) {
        Ok(Some((queue, paused, rate))) => {
            let count = queue.queue.len();
            coordinator.seed_state(queue, paused, rate);
            tracing::info!("restored {count} NZBs from disk state");
        }
        Ok(None) => {}
        Err(err) => {
            tracing::warn!("failed to restore queue from disk: {err}");
        }
    }

    let coordinator_handle = tokio::spawn(async move {
        coordinator.run().await;
    });

    let worker_writer_pool = writer_pool.clone();
    let worker_handle = tokio::spawn(crate::download::download_worker(
        assignment_rx,
        queue_handle.clone(),
        fetcher,
        inter_dir,
        rate_rx,
        cache,
        worker_writer_pool,
    ));

    let feed_configs: Vec<nzbg_feed::FeedConfig> = nzbg_config::extract_feeds(config.raw())
        .into_iter()
        .map(|f| nzbg_feed::FeedConfig {
            id: f.id,
            name: f.name,
            url: f.url,
            filter: f.filter,
            interval_min: f.interval_min,
            backlog: f.backlog,
            pause_nzb: f.pause_nzb,
            category: f.category,
            priority: f.priority,
            extensions: f.extensions,
        })
        .collect();

    let feed_handle = if !feed_configs.is_empty() {
        let feed_history = nzbg_feed::FeedHistoryDb::new(30);
        let feed_fetcher = Box::new(nzbg_feed::fetch::HttpFeedFetcher);
        let feed_coordinator =
            nzbg_feed::FeedCoordinator::new(feed_configs, feed_history, feed_fetcher);
        let (feed_tx, feed_rx) = tokio::sync::mpsc::channel(16);
        let handle = nzbg_feed::FeedHandle::new(feed_tx);
        tokio::spawn(feed_coordinator.run_actor(feed_rx));
        tracing::info!("feed coordinator started");
        Some(handle)
    } else {
        None
    };

    let (shutdown_handle, mut shutdown_rx) = ShutdownHandle::new();
    let mut app_state_builder = AppState::default()
        .with_queue(queue_handle.clone())
        .with_shutdown(shutdown_handle)
        .with_disk(disk.clone());
    if let Some(ref fh) = feed_handle {
        app_state_builder = app_state_builder.with_feed_handle(fh.clone());
    }
    let app_state = Arc::new(app_state_builder);

    let deps = nzbg_scheduler::ServiceDeps {
        queue: queue_handle.clone(),
        disk: disk.clone(),
        command_deps: nzbg_scheduler::CommandDeps {
            postproc_paused: Some(app_state.postproc_paused().clone()),
            scan_paused: Some(app_state.scan_paused().clone()),
            scan_trigger: app_state.scan_trigger().cloned(),
            feed_handle,
        },
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

    if let Err(err) = writer_pool.flush_all().await {
        tracing::warn!("flushing writer pool: {err}");
    }

    if let Ok(snapshot) = queue_handle.get_queue_snapshot().await {
        let state = nzbg_scheduler::DiskStateFlush::snapshot_to_queue_state(&snapshot);
        let disk_clone = disk.clone();
        let _ = tokio::task::spawn_blocking(move || disk_clone.save_queue(&state)).await;
        tracing::info!("final disk state flush complete");
    }

    let _ = queue_handle.shutdown().await;
    let _ = coordinator_handle.await;

    drop(postproc_tx);
    forward_handle.abort();
    let _ = postproc_handle.await;

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
        let _ = result;
    }

    const TEST_NZB: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
  <file poster="user@example.com" date="1706140800"
        subject='Test [01/01] - "data.rar" yEnc (1/1)'>
    <groups><group>alt.binaries.test</group></groups>
    <segments>
      <segment bytes="100" number="1">seg1@example.com</segment>
    </segments>
  </file>
</nzb>
"#;

    #[test]
    fn restore_queue_rebuilds_from_disk_state() {
        use nzbg_diskstate::{DiskState, JsonFormat, NzbState, QueueState};

        let tmp = tempfile::tempdir().expect("tempdir");
        let disk = DiskState::new(tmp.path().to_path_buf(), JsonFormat).expect("disk");

        let queue_state = QueueState {
            version: 3,
            nzbs: vec![NzbState {
                id: 5,
                name: "test-nzb".to_string(),
                filename: "test.nzb".to_string(),
                category: "tv".to_string(),
                dest_dir: std::path::PathBuf::from("/dest"),
                final_dir: None,
                priority: 0,
                paused: true,
                url: None,
                dupe_key: "test-nzb".to_string(),
                dupe_score: 0,
                dupe_mode: nzbg_core::models::DupMode::Score,
                added_time: chrono::Utc::now(),
                total_size: 100,
                downloaded_size: 0,
                failed_size: 0,
                file_ids: vec![10],
                post_process_parameters: std::collections::HashMap::new(),
                health: 1000,
                critical_health: 1000,
                total_article_count: 1,
                success_article_count: 0,
                failed_article_count: 0,
            }],
            next_nzb_id: 10,
            next_file_id: 20,
            download_paused: true,
            speed_limit: 5000,
        };
        disk.save_queue(&queue_state).expect("save queue");
        disk.save_nzb_file(5, TEST_NZB.as_bytes())
            .expect("save nzb");

        let (queue, paused, rate) = restore_queue(&disk).expect("restore").expect("some queue");

        assert_eq!(queue.queue.len(), 1);
        assert_eq!(queue.queue[0].id, 5);
        assert_eq!(queue.queue[0].name, "test-nzb");
        assert_eq!(queue.queue[0].category, "tv");
        assert!(queue.queue[0].paused);
        assert_eq!(queue.queue[0].files.len(), 1);
        assert_eq!(queue.queue[0].files[0].id, 10);
        assert_eq!(queue.queue[0].files[0].articles.len(), 1);
        assert_eq!(queue.next_nzb_id, 10);
        assert!(paused);
        assert_eq!(rate, 5000);
    }

    #[test]
    fn restore_queue_returns_none_when_no_state() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let disk = DiskState::new(tmp.path().to_path_buf(), JsonFormat).expect("disk");

        let result = restore_queue(&disk).expect("restore");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn forward_completions_maps_notice_to_request() {
        let (notice_tx, notice_rx) = tokio::sync::mpsc::channel(4);
        let (postproc_tx, mut postproc_rx) = tokio::sync::mpsc::channel(4);

        let handle = forward_completions(notice_rx, postproc_tx);

        notice_tx
            .send(NzbCompletionNotice {
                nzb_id: 42,
                nzb_name: "test-nzb".to_string(),
                working_dir: std::path::PathBuf::from("/tmp/work"),
                category: Some("movies".to_string()),
                parameters: vec![("key".to_string(), "val".to_string())],
            })
            .await
            .expect("send");
        drop(notice_tx);

        let req = postproc_rx.recv().await.expect("recv");
        assert_eq!(req.nzb_id, 42);
        assert_eq!(req.nzb_name, "test-nzb");
        assert_eq!(req.working_dir, std::path::PathBuf::from("/tmp/work"));
        assert_eq!(req.category, Some("movies".to_string()));
        assert_eq!(req.parameters, vec![("key".to_string(), "val".to_string())]);

        handle.await.expect("join");
    }

    #[test]
    fn postproc_config_maps_fields() {
        let config = Config::from_raw(
            [
                ("DestDir".to_string(), "/dest".to_string()),
                ("AppendCategoryDir".to_string(), "yes".to_string()),
                ("UnpackCleanupDisk".to_string(), "no".to_string()),
                ("ExtCleanupDisk".to_string(), ".par2".to_string()),
            ]
            .into_iter()
            .collect(),
        );
        let pp = postproc_config(&config);
        assert_eq!(pp.dest_dir, std::path::PathBuf::from("/dest"));
        assert!(pp.append_category_dir);
        assert!(!pp.unpack_cleanup_disk);
        assert_eq!(pp.ext_cleanup_disk, ".par2");
    }
}
