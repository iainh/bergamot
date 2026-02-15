use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use bergamot_config::{Config, parse_config};
use bergamot_core::models::{
    ArticleInfo, ArticleStatus, DownloadQueue, FileInfo, HistoryInfo, HistoryKind, NzbInfo,
    Priority,
};
use bergamot_diskstate::{DiskState, JsonFormat, StateLock};
use bergamot_extension::{
    ExtensionInfo, ExtensionKind, ExtensionManager, ExtensionRunner, NzbppContext, ProcessRunner,
    build_nzbpp_env,
};
use bergamot_logging::{BufferLayer, LogBuffer};
use bergamot_postproc::{
    ExtensionContext, ExtensionExecutor, PostProcessRequest, PostProcessor, PostStatusReporter,
};
use bergamot_queue::NzbCompletionNotice;
use bergamot_server::{
    AppState, ServerConfig as WebServerConfig, ShutdownHandle, WebServer, spawn_stats_updater,
};

pub fn load_config(path: &Path, overrides: &[String]) -> Result<Config> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("reading config: {}", path.display()))?;
    let mut raw = parse_config(&content).context("parsing config")?;
    for opt in overrides {
        let (key, value) = opt
            .split_once('=')
            .with_context(|| format!("invalid option '{opt}': expected KEY=VALUE"))?;
        raw.insert(key.trim().to_string(), value.trim().to_string());
    }
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
        secure_port: config.secure_port,
        secure_cert: config.secure_cert.clone(),
        secure_key: config.secure_key.clone(),
        cert_store: config.cert_store.clone(),
        form_auth: config.form_auth,
        authorized_ips: config.authorized_ip.clone(),
        control_username: config.control_username.clone(),
        control_password: config.control_password.clone(),
        restricted_username: config.restricted_username.clone(),
        restricted_password: config.restricted_password.clone(),
        add_username: config.add_username.clone(),
        add_password: config.add_password.clone(),
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
        let parsed = match bergamot_nzb::parse_nzb_auto(&nzb_bytes) {
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

            let mut articles: Vec<ArticleInfo> = nzb_file
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

            let mut success_articles: u32 = 0;
            let mut success_size: u64 = 0;
            let mut file_completed = false;

            if let Ok(file_state) = disk.load_file_state(file_id) {
                for (idx, article) in articles.iter_mut().enumerate() {
                    let idx = idx as u32;
                    if file_state.is_article_done(idx) {
                        article.status = ArticleStatus::Finished;
                        if let Some(&crc) = file_state.article_crcs.get(&idx) {
                            article.crc = crc;
                        }
                        success_articles += 1;
                        success_size += article.size;
                    }
                }
                if success_articles == article_count {
                    file_completed = true;
                }
            }

            let remaining_size = file_size.saturating_sub(success_size);

            files.push(FileInfo {
                id: file_id,
                nzb_id: nzb_state.id,
                filename: output_filename.clone(),
                subject: nzb_file.subject.clone(),
                output_filename,
                groups: nzb_file.groups.clone(),
                articles,
                size: file_size,
                remaining_size,
                success_size,
                failed_size: 0,
                missed_size: 0,
                total_articles: article_count,
                missing_articles: 0,
                failed_articles: 0,
                success_articles,
                paused: false,
                completed: file_completed,
                priority,
                time: std::time::SystemTime::now(),
                active_downloads: 0,
                crc: 0,
                server_stats: vec![],
            });
        }

        let nzb_success_size: u64 = files.iter().map(|f| f.success_size).sum();
        let nzb_success_articles: u32 = files.iter().map(|f| f.success_articles).sum();
        let nzb_remaining_size = total_size.saturating_sub(nzb_success_size);
        let completed_file_count = files.iter().filter(|f| f.completed).count() as u32;
        let remaining_file_count = files.len() as u32 - completed_file_count;

        let nzb = NzbInfo {
            id: nzb_state.id,
            kind: bergamot_core::models::NzbKind::Nzb,
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
            remaining_size: nzb_remaining_size,
            paused_size: 0,
            failed_size: 0,
            success_size: nzb_success_size,
            current_downloaded_size: 0,
            par_size: 0,
            par_remaining_size: 0,
            par_current_success_size: 0,
            par_failed_size: 0,
            par_total_article_count: 0,
            par_failed_article_count: 0,
            file_count: files.len() as u32,
            remaining_file_count,
            remaining_par_count: 0,
            total_article_count,
            success_article_count: nzb_success_articles,
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
            par_status: bergamot_core::models::ParStatus::None,
            unpack_status: bergamot_core::models::UnpackStatus::None,
            move_status: bergamot_core::models::MoveStatus::None,
            delete_status: bergamot_core::models::DeleteStatus::None,
            mark_status: bergamot_core::models::MarkStatus::None,
            url_status: bergamot_core::models::UrlStatus::None,
            script_status: bergamot_core::models::ScriptStatus::None,
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

    let history = restore_history(disk);

    let queue = DownloadQueue {
        queue: queue_nzbs,
        history,
        next_nzb_id: state.next_nzb_id,
        next_file_id: max_file_id,
    };

    Ok(Some((queue, state.download_paused, state.speed_limit)))
}

fn restore_history(disk: &DiskState<JsonFormat>) -> Vec<HistoryInfo> {
    let history_state = match disk.load_history() {
        Ok(s) => s,
        Err(err) => {
            if err
                .downcast_ref::<std::io::Error>()
                .is_none_or(|e| e.kind() != std::io::ErrorKind::NotFound)
            {
                tracing::warn!("failed to load history: {err}");
            }
            return Vec::new();
        }
    };

    let mut entries = Vec::with_capacity(history_state.entries.len());
    for h in &history_state.entries {
        let kind = match h.kind {
            0 => HistoryKind::Nzb,
            1 => HistoryKind::Url,
            2 => HistoryKind::DupHidden,
            _ => HistoryKind::Nzb,
        };
        let time = std::time::SystemTime::from(h.completed_at);
        let nzb_info = NzbInfo {
            id: h.id,
            kind: bergamot_core::models::NzbKind::Nzb,
            name: h.name.clone(),
            filename: h.nzb_filename.clone(),
            url: String::new(),
            dest_dir: std::path::PathBuf::from(&h.dest_dir),
            final_dir: std::path::PathBuf::from(&h.final_dir),
            temp_dir: std::path::PathBuf::new(),
            queue_dir: std::path::PathBuf::new(),
            category: h.category.clone(),
            priority: Priority::Normal,
            dup_key: h.dupe_key.clone(),
            dup_mode: h.dupe_mode,
            dup_score: h.dupe_score,
            size: h.size,
            remaining_size: 0,
            paused_size: 0,
            failed_size: 0,
            success_size: h.size,
            current_downloaded_size: 0,
            par_size: 0,
            par_remaining_size: 0,
            par_current_success_size: 0,
            par_failed_size: 0,
            par_total_article_count: 0,
            par_failed_article_count: 0,
            file_count: 0,
            remaining_file_count: 0,
            remaining_par_count: 0,
            total_article_count: 0,
            success_article_count: 0,
            failed_article_count: 0,
            added_time: time,
            min_time: None,
            max_time: None,
            download_start_time: None,
            download_sec: 0,
            post_total_sec: 0,
            par_sec: 0,
            repair_sec: 0,
            unpack_sec: 0,
            paused: false,
            deleted: false,
            direct_rename: false,
            force_priority: false,
            reprocess: false,
            par_manual: false,
            clean_up_disk: false,
            par_status: bergamot_core::models::ParStatus::None,
            unpack_status: bergamot_core::models::UnpackStatus::None,
            move_status: bergamot_core::models::MoveStatus::None,
            delete_status: bergamot_core::models::DeleteStatus::None,
            mark_status: bergamot_core::models::MarkStatus::None,
            url_status: bergamot_core::models::UrlStatus::None,
            script_status: bergamot_core::models::ScriptStatus::None,
            health: 1000,
            critical_health: 1000,
            files: vec![],
            completed_files: vec![],
            server_stats: vec![],
            parameters: h.parameters.iter().map(|(k, v)| bergamot_core::models::NzbParameter { name: k.clone(), value: v.clone() }).collect(),
            post_info: None,
            message_count: 0,
            cached_message_count: 0,
        };
        entries.push(HistoryInfo {
            id: h.id,
            kind,
            time,
            nzb_info,
        });
    }

    tracing::info!("restored {} history entries", entries.len());
    entries
}

fn postproc_config(config: &Config) -> bergamot_postproc::Config {
    bergamot_postproc::Config {
        par2_path: std::path::PathBuf::from("par2"),
        dest_dir: config.dest_dir.clone(),
        append_category_dir: config.append_category_dir,
        password_file: None,
        unpack_cleanup_disk: config.unpack_cleanup_disk,
        ext_cleanup_disk: config.ext_cleanup_disk.clone(),
    }
}

struct PostProcessExtensionExecutor<R: ProcessRunner> {
    runner: Arc<ExtensionRunner<R>>,
    extensions: Vec<ExtensionInfo>,
    dest_dir: std::path::PathBuf,
}

#[async_trait::async_trait]
impl<R: ProcessRunner + 'static> ExtensionExecutor for PostProcessExtensionExecutor<R> {
    async fn run_post_process(
        &self,
        ctx: &ExtensionContext,
    ) -> Result<(), bergamot_postproc::PostProcessError> {
        let nzbpp = build_nzbpp_env(&NzbppContext {
            nzb_name: &ctx.nzb_name,
            nzb_id: ctx.nzb_id,
            category: &ctx.category,
            working_dir: &ctx.working_dir,
            dest_dir: &self.dest_dir,
            final_dir: &ctx.working_dir,
            total_status: "SUCCESS",
            par_status: &ctx.par_status,
            unpack_status: &ctx.unpack_status,
            parameters: &ctx.parameters,
        });
        for ext in &self.extensions {
            if !ext.enabled {
                continue;
            }
            match self
                .runner
                .run_post_process(ext, &nzbpp, &ctx.working_dir)
                .await
            {
                Ok(result) => {
                    tracing::info!(
                        extension = %ext.metadata.name,
                        exit_code = result.exit_code,
                        "post-processing extension completed"
                    );
                }
                Err(err) => {
                    tracing::warn!(
                        extension = %ext.metadata.name,
                        error = %err,
                        "extension failed to run"
                    );
                }
            }
        }
        Ok(())
    }
}

struct QueuePostReporter {
    queue: bergamot_queue::QueueHandle,
}

#[async_trait::async_trait]
impl PostStatusReporter for QueuePostReporter {
    async fn report_stage(&self, nzb_id: u32, stage: bergamot_core::models::PostStage) {
        let _ = self.queue.update_post_stage(nzb_id, stage).await;
    }

    async fn report_progress(&self, nzb_id: u32, progress: u32) {
        let _ = self.queue.update_post_progress(nzb_id, progress).await;
    }

    async fn report_done(
        &self,
        nzb_id: u32,
        par: bergamot_core::models::ParStatus,
        unpack: bergamot_core::models::UnpackStatus,
        mv: bergamot_core::models::MoveStatus,
        final_dir: Option<std::path::PathBuf>,
        timings: bergamot_postproc::PostTimings,
    ) {
        let qt = bergamot_queue::PostProcessTimings {
            post_total_sec: timings.total_sec,
            par_sec: timings.par_sec,
            repair_sec: timings.repair_sec,
            unpack_sec: timings.unpack_sec,
        };
        let _ = self
            .queue
            .finish_post_processing(nzb_id, par, unpack, mv, final_dir, qt)
            .await;
    }
}

fn scan_script_dir(script_dir: &Path) -> Vec<ExtensionInfo> {
    let entries = match std::fs::read_dir(script_dir) {
        Ok(e) => e,
        Err(err) => {
            tracing::debug!("cannot read script dir {}: {err}", script_dir.display());
            return Vec::new();
        }
    };
    let mut extensions = Vec::new();
    for (order, entry) in entries.filter_map(|e| e.ok()).enumerate() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if path
                .metadata()
                .is_ok_and(|meta| meta.permissions().mode() & 0o111 == 0)
            {
                continue;
            }
        }
        let name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        extensions.push(ExtensionInfo {
            metadata: bergamot_extension::ExtensionMetadata {
                name: name.clone(),
                display_name: name,
                description: String::new(),
                kind: vec![ExtensionKind::PostProcessing],
                parameters: Vec::new(),
                author: None,
                homepage: None,
                version: None,
                nzbget_min_version: None,
            },
            path,
            enabled: true,
            order: order as u32,
        });
    }
    extensions.sort_by_key(|e| e.order);
    extensions
}

fn build_extension_executor(config: &Config) -> Option<Arc<dyn ExtensionExecutor>> {
    let script_dir = &config.script_dir;
    let extensions = scan_script_dir(script_dir);
    if extensions.is_empty() {
        return None;
    }
    tracing::info!(
        count = extensions.len(),
        dir = %script_dir.display(),
        "loaded post-processing extensions"
    );
    let manager = ExtensionManager::new(vec![script_dir.clone()]);
    let runner = Arc::new(ExtensionRunner::new(
        bergamot_extension::RealProcessRunner,
        manager,
    ));
    Some(Arc::new(PostProcessExtensionExecutor {
        runner,
        extensions,
        dest_dir: config.dest_dir.clone(),
    }))
}

pub fn forward_completions(
    mut completion_rx: tokio::sync::mpsc::Receiver<NzbCompletionNotice>,
    postproc_tx: tokio::sync::mpsc::Sender<PostProcessRequest>,
    writer_pool: Arc<crate::writer::FileWriterPool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(notice) = completion_rx.recv().await {
            if let Err(err) = writer_pool.flush_prefix(&notice.working_dir).await {
                tracing::warn!(
                    nzb = %notice.nzb_name,
                    error = %err,
                    "flushing writer pool for completed NZB"
                );
            }
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

pub async fn run_with_config_path(
    config: Config,
    fetcher: Arc<dyn crate::download::ArticleFetcher>,
    config_path: Option<std::path::PathBuf>,
    log_buffer: Option<Arc<LogBuffer>>,
    shared_stats: Option<Arc<bergamot_scheduler::SharedStatsTracker>>,
) -> Result<()> {
    let web_config = Arc::new(web_server_config(&config));
    let inter_dir = config.inter_dir.clone();

    let (cache, cache_bytes_ref): (
        Arc<dyn crate::cache::ArticleCache>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) = if config.article_cache > 0 {
        let bounded = crate::cache::BoundedCache::new(config.article_cache as usize * 1024 * 1024);
        let bytes_ref = bounded.current_bytes_ref().clone();
        (Arc::new(bounded), bytes_ref)
    } else {
        (
            Arc::new(crate::cache::NoopCache),
            Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        )
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

    let total_connections = config
        .servers
        .iter()
        .map(|s| s.connections.max(1) as usize)
        .sum::<usize>()
        .max(1);

    // Build a server scheduler from the configured news servers.
    // Each server becomes a slot in the weighted fair queuing scheduler,
    // with initial weights proportional to their connection count.
    // As downloads complete, the scheduler adapts weights using EWMA
    // throughput estimation.
    let server_slots: Vec<bergamot_nntp::ServerSlot> = config
        .servers
        .iter()
        .filter(|s| s.active)
        .map(|s| {
            bergamot_nntp::ServerSlot::new(s.id, s.name.clone(), s.level, s.connections.max(1))
        })
        .collect();
    let server_scheduler = bergamot_nntp::ServerScheduler::new(server_slots);

    let (coordinator, queue_handle, assignment_rx, rate_rx) = bergamot_queue::QueueCoordinator::new(
        total_connections,
        total_connections,
        inter_dir.clone(),
        config.dest_dir.clone(),
    );
    let mut coordinator = coordinator
        .with_completion_tx(completion_tx)
        .with_server_scheduler(server_scheduler);

    let mut restored_paused = false;
    match restore_queue(&disk) {
        Ok(Some((queue, paused, rate))) => {
            let count = queue.queue.len();
            restored_paused = paused;
            coordinator.seed_state(queue, paused, rate);
            tracing::info!("restored {count} downloads from saved state");
        }
        Ok(None) => {}
        Err(err) => {
            tracing::warn!("failed to restore queue from disk: {err}");
        }
    }

    let coordinator_handle = tokio::spawn(async move {
        coordinator.run().await;
    });

    let pp_config = Arc::new(postproc_config(&config));
    let par2 = Arc::new(bergamot_postproc::NativePar2Engine);
    let unpacker = Arc::new(bergamot_postproc::CommandLineUnpacker);
    let history = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut postprocessor = PostProcessor::new(postproc_rx, pp_config, history, par2, unpacker);
    if let Some(executor) = build_extension_executor(&config) {
        postprocessor = postprocessor.with_extensions(executor);
    }
    let reporter = Arc::new(QueuePostReporter {
        queue: queue_handle.clone(),
    });
    postprocessor = postprocessor.with_reporter(reporter);
    let postproc_handle = tokio::spawn(async move {
        postprocessor.run().await;
    });
    let forward_handle = forward_completions(completion_rx, postproc_tx.clone(), writer_pool.clone());

    let worker_writer_pool = writer_pool.clone();
    let worker_handle = tokio::spawn(crate::download::download_worker(
        assignment_rx,
        queue_handle.clone(),
        fetcher,
        inter_dir,
        rate_rx,
        cache,
        worker_writer_pool,
        crate::download::MAX_CONCURRENT_DOWNLOADS,
    ));

    let feed_configs: Vec<bergamot_feed::FeedConfig> = bergamot_config::extract_feeds(config.raw())
        .into_iter()
        .map(|f| bergamot_feed::FeedConfig {
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
        let feed_history = bergamot_feed::FeedHistoryDb::new(30);
        let feed_fetcher = Box::new(bergamot_feed::fetch::HttpFeedFetcher);
        let feed_coordinator =
            bergamot_feed::FeedCoordinator::new(feed_configs, feed_history, feed_fetcher);
        let (feed_tx, feed_rx) = tokio::sync::mpsc::channel(16);
        let handle = bergamot_feed::FeedHandle::new(feed_tx);
        tokio::spawn(feed_coordinator.run_actor(feed_rx));
        tracing::info!("RSS feed monitoring started");
        Some(handle)
    } else {
        None
    };

    let (shutdown_handle, shutdown_token) = ShutdownHandle::new();
    let config_arc = std::sync::Arc::new(std::sync::RwLock::new(config));
    let mut app_state_builder = AppState::default()
        .with_queue(queue_handle.clone())
        .with_shutdown(shutdown_handle)
        .with_disk(disk.clone())
        .with_article_cache_bytes(cache_bytes_ref);
    app_state_builder
        .download_paused()
        .store(restored_paused, std::sync::atomic::Ordering::Relaxed);
    if let Some(cp) = config_path {
        app_state_builder = app_state_builder.with_config(config_arc.clone(), cp);
    }
    if let Some(lb) = log_buffer {
        app_state_builder = app_state_builder.with_log_buffer(lb);
    }
    if let Some(ref fh) = feed_handle {
        app_state_builder = app_state_builder.with_feed_handle(fh.clone());
    }
    if let Some(ref ss) = shared_stats {
        app_state_builder = app_state_builder.with_stats_tracker(ss.clone());
    }
    let app_state = Arc::new(app_state_builder);

    let deps = bergamot_scheduler::ServiceDeps {
        queue: queue_handle.clone(),
        disk: disk.clone(),
        command_deps: bergamot_scheduler::CommandDeps {
            postproc_paused: Some(app_state.postproc_paused().clone()),
            scan_paused: Some(app_state.scan_paused().clone()),
            scan_trigger: app_state.scan_trigger().cloned(),
            feed_handle,
            server_pool: None,
        },
        stats_recorder: shared_stats,
    };
    let config_snapshot = config_arc
        .read()
        .expect("config lock poisoned")
        .clone();
    let (scheduler_tx, scheduler_handles) =
        bergamot_scheduler::start_services(&config_snapshot, deps).await?;

    let stats_handle = spawn_stats_updater(app_state.clone(), queue_handle.clone());

    let server = WebServer::new(web_config, app_state);
    let server_handle = tokio::spawn(async move {
        if let Err(err) = server.run().await {
            tracing::error!("web server error: {err}");
        }
    });

    tokio::select! {
        biased;
        _ = shutdown_token.cancelled() => {
            tracing::info!("shutdown requested via API");
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("shutdown requested (Ctrl+C)");
        }
    }

    bergamot_scheduler::shutdown_services(scheduler_tx, scheduler_handles).await;

    if let Ok(snapshot) = queue_handle.get_queue_snapshot().await {
        let state = bergamot_scheduler::DiskStateFlush::snapshot_to_queue_state(&snapshot);
        let history_state =
            bergamot_scheduler::DiskStateFlush::snapshot_to_history_state(&snapshot);
        let article_states = queue_handle.get_all_file_article_states().await.ok();
        let disk_clone = disk.clone();
        let _ = tokio::task::spawn_blocking(move || {
            disk_clone.save_queue(&state)?;
            disk_clone.save_history(&history_state)?;
            if let Some(states) = article_states {
                for snap in &states {
                    let mut file_state = bergamot_diskstate::FileArticleState::new(
                        snap.file_id,
                        snap.total_articles,
                    );
                    for &(idx, crc) in &snap.completed_articles {
                        file_state.mark_article_done(idx, crc);
                    }
                    disk_clone.save_file_state(snap.file_id, &file_state)?;
                }
            }
            Ok::<(), anyhow::Error>(())
        })
        .await;
        tracing::info!("saved download state to disk");
    }

    let _ = queue_handle.shutdown().await;
    let _ = coordinator_handle.await;

    drop(postproc_tx);

    if let Err(err) = writer_pool.flush_all().await {
        tracing::warn!("flushing writer pool: {err}");
    }

    server_handle.abort();

    let shutdown_timeout = std::time::Duration::from_secs(10);
    let handles: Vec<(&str, tokio::task::JoinHandle<()>)> = vec![
        ("stats_updater", stats_handle),
        ("download_worker", worker_handle),
        ("forward_completions", forward_handle),
        ("post_processor", postproc_handle),
    ];

    for (name, handle) in handles {
        match tokio::time::timeout(shutdown_timeout, handle).await {
            Ok(Ok(())) => tracing::debug!("{name} stopped cleanly"),
            Ok(Err(err)) if err.is_panic() => {
                tracing::error!("{name} panicked during shutdown: {err}")
            }
            Ok(Err(err)) => tracing::debug!("{name} cancelled: {err}"),
            Err(elapsed) => {
                tracing::warn!("{name} did not stop within {elapsed}, aborting");
            }
        }
    }

    tracing::info!("bergamot stopped");
    Ok(())
}

pub fn default_config_path() -> Option<std::path::PathBuf> {
    let candidates = [
        dirs::config_dir().map(|d| d.join("bergamot").join("bergamot.conf")),
        Some(std::path::PathBuf::from("/etc/bergamot.conf")),
        Some(std::path::PathBuf::from("/usr/local/etc/bergamot.conf")),
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
        writeln!(tmp, "MainDir=/tmp/bergamot-test").expect("write");
        writeln!(tmp, "ControlPort=6790").expect("write");

        let config = load_config(tmp.path(), &[]).expect("load");
        assert_eq!(config.control_port, 6790);
        assert_eq!(
            config.main_dir,
            std::path::PathBuf::from("/tmp/bergamot-test")
        );
    }

    #[test]
    fn load_config_returns_error_for_missing_file() {
        let result = load_config(Path::new("/nonexistent/bergamot.conf"), &[]);
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
        use bergamot_diskstate::{DiskState, JsonFormat, NzbState, QueueState};

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
                dupe_mode: bergamot_core::models::DupMode::Score,
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

    const TEST_NZB_MULTI_SEG: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
  <file poster="user@example.com" date="1706140800"
        subject='Test [01/01] - "data.rar" yEnc (1/3)'>
    <groups><group>alt.binaries.test</group></groups>
    <segments>
      <segment bytes="100" number="1">seg1@example.com</segment>
      <segment bytes="200" number="2">seg2@example.com</segment>
      <segment bytes="300" number="3">seg3@example.com</segment>
    </segments>
  </file>
</nzb>
"#;

    #[test]
    fn restore_queue_loads_file_article_states() {
        use bergamot_diskstate::{DiskState, FileArticleState, JsonFormat, NzbState, QueueState};

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
                paused: false,
                url: None,
                dupe_key: "test-nzb".to_string(),
                dupe_score: 0,
                dupe_mode: bergamot_core::models::DupMode::Score,
                added_time: chrono::Utc::now(),
                total_size: 600,
                downloaded_size: 0,
                failed_size: 0,
                file_ids: vec![10],
                post_process_parameters: std::collections::HashMap::new(),
                health: 1000,
                critical_health: 1000,
                total_article_count: 3,
                success_article_count: 0,
                failed_article_count: 0,
            }],
            next_nzb_id: 10,
            next_file_id: 20,
            download_paused: false,
            speed_limit: 0,
        };
        disk.save_queue(&queue_state).expect("save queue");
        disk.save_nzb_file(5, TEST_NZB_MULTI_SEG.as_bytes())
            .expect("save nzb");

        let mut file_state = FileArticleState::new(10, 3);
        file_state.mark_article_done(0, 0xAAAA);
        file_state.mark_article_done(2, 0xCCCC);
        disk.save_file_state(10, &file_state)
            .expect("save file state");

        let (queue, _paused, _rate) = restore_queue(&disk).expect("restore").expect("some queue");

        let file = &queue.queue[0].files[0];
        assert_eq!(file.success_articles, 2);
        assert_eq!(file.success_size, 400);
        assert_eq!(file.remaining_size, 200);
        assert!(!file.completed);
        assert_eq!(file.articles[0].status, ArticleStatus::Finished);
        assert_eq!(file.articles[0].crc, 0xAAAA);
        assert_eq!(file.articles[1].status, ArticleStatus::Undefined);
        assert_eq!(file.articles[2].status, ArticleStatus::Finished);
        assert_eq!(file.articles[2].crc, 0xCCCC);

        assert_eq!(queue.queue[0].success_size, 400);
        assert_eq!(queue.queue[0].remaining_size, 200);
        assert_eq!(queue.queue[0].success_article_count, 2);
    }

    #[test]
    fn restore_queue_marks_file_completed_when_all_articles_done() {
        use bergamot_diskstate::{DiskState, FileArticleState, JsonFormat, NzbState, QueueState};

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
                paused: false,
                url: None,
                dupe_key: "test-nzb".to_string(),
                dupe_score: 0,
                dupe_mode: bergamot_core::models::DupMode::Score,
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
            download_paused: false,
            speed_limit: 0,
        };
        disk.save_queue(&queue_state).expect("save queue");
        disk.save_nzb_file(5, TEST_NZB.as_bytes())
            .expect("save nzb");

        let mut file_state = FileArticleState::new(10, 1);
        file_state.mark_article_done(0, 0xBBBB);
        disk.save_file_state(10, &file_state)
            .expect("save file state");

        let (queue, _paused, _rate) = restore_queue(&disk).expect("restore").expect("some queue");

        let file = &queue.queue[0].files[0];
        assert!(file.completed);
        assert_eq!(file.success_articles, 1);
        assert_eq!(queue.queue[0].remaining_file_count, 0);
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

        let pool = Arc::new(crate::writer::FileWriterPool::new());
        let handle = forward_completions(notice_rx, postproc_tx, pool);

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
