use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use tokio::sync::broadcast;

use nzbg_config::Config;

#[async_trait]
pub trait Service: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn interval(&self) -> Duration;
    async fn tick(&mut self) -> anyhow::Result<()>;
}

pub async fn run_service(mut service: Box<dyn Service>, mut shutdown: broadcast::Receiver<()>) {
    let name = service.name().to_string();
    tracing::info!("starting service: {name}");

    let mut interval = tokio::time::interval(service.interval());
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = interval.tick() => {
                if let Err(err) = service.tick().await {
                    tracing::error!("service {name} error: {err}");
                }
            }
            _ = shutdown.recv() => {
                tracing::info!("shutting down service: {name}");
                break;
            }
        }
    }
}

pub struct ServiceDeps {
    pub queue: nzbg_queue::QueueHandle,
    pub disk: std::sync::Arc<nzbg_diskstate::DiskState<nzbg_diskstate::JsonFormat>>,
    pub command_deps: CommandDeps,
    pub stats_recorder: Option<std::sync::Arc<SharedStatsTracker>>,
}

pub async fn start_services(
    config: &Config,
    deps: ServiceDeps,
) -> anyhow::Result<(broadcast::Sender<()>, Vec<tokio::task::JoinHandle<()>>)> {
    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    let services = build_services(config, deps)?;
    let mut handles = Vec::with_capacity(services.len());

    for service in services {
        let rx = shutdown_tx.subscribe();
        handles.push(tokio::spawn(run_service(service, rx)));
    }

    Ok((shutdown_tx, handles))
}

pub async fn shutdown_services(
    tx: broadcast::Sender<()>,
    handles: Vec<tokio::task::JoinHandle<()>>,
) {
    let _ = tx.send(());
    for handle in handles {
        let _ = handle.await;
    }
    tracing::info!("all services stopped");
}

fn build_services(config: &Config, deps: ServiceDeps) -> anyhow::Result<Vec<Box<dyn Service>>> {
    let server_pool = deps.command_deps.server_pool.clone();
    let executor = CommandExecutor::new(deps.queue.clone()).with_deps(deps.command_deps);
    let scheduler = Scheduler::from_config_with_executor(config, deps.queue.clone(), executor)?;
    let scanner = NzbDirScanner::from_config(config)
        .with_queue(deps.queue.clone())
        .with_disk(deps.disk.clone());
    let disk_space = DiskSpaceMonitor::from_config(config).with_queue(deps.queue.clone());
    let history = HistoryCleanup::from_config(config).with_queue(deps.queue.clone());
    let stats: Box<dyn Service> = match deps.stats_recorder {
        Some(shared) => Box::new(ServiceSharedStats(shared)),
        None => Box::new(StatsTracker::from_config(config)),
    };
    let health = HealthChecker::from_config(config)
        .with_disk(deps.disk.clone())
        .with_queue(deps.queue.clone());
    let disk_flush = DiskStateFlush::new(deps.queue, deps.disk);

    let mut conn_cleanup = ConnectionCleanup::new();
    if let Some(pool) = server_pool {
        conn_cleanup = conn_cleanup.with_pool(pool);
    }

    Ok(vec![
        Box::new(scheduler),
        Box::new(scanner),
        Box::new(disk_space),
        Box::new(conn_cleanup),
        Box::new(history),
        stats,
        Box::new(health),
        Box::new(disk_flush),
    ])
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulerCommand {
    PauseDownload,
    UnpauseDownload,
    PausePostProcess,
    UnpausePostProcess,
    DownloadRate,
    Extensions,
    Process,
    PauseScan,
    UnpauseScan,
    ActivateServer,
    DeactivateServer,
    FetchFeed,
}

#[derive(Debug, Clone)]
pub struct ScheduledTask {
    pub id: u32,
    pub time: NaiveTime,
    pub weekdays: u8,
    pub command: SchedulerCommand,
    pub param: String,
    pub last_executed: Option<NaiveDateTime>,
}

impl ScheduledTask {
    pub fn should_execute(&self, now: NaiveDateTime) -> bool {
        let weekday_bit = 1u8 << now.weekday().num_days_from_monday();
        if self.weekdays & weekday_bit == 0 {
            return false;
        }

        if now.time().hour() != self.time.hour() || now.time().minute() != self.time.minute() {
            return false;
        }

        if let Some(last) = self.last_executed
            && last.date() == now.date()
            && last.time().hour() == now.time().hour()
            && last.time().minute() == now.time().minute()
        {
            return false;
        }

        true
    }
}

pub struct Scheduler {
    tasks: Vec<ScheduledTask>,
    executor: CommandExecutor,
    clock: Box<dyn Clock>,
}

impl Scheduler {
    pub fn new(tasks: Vec<ScheduledTask>, queue: nzbg_queue::QueueHandle) -> Self {
        Self {
            tasks,
            executor: CommandExecutor::new(queue),
            clock: Box::new(SystemClock),
        }
    }

    pub fn from_config(config: &Config, queue: nzbg_queue::QueueHandle) -> anyhow::Result<Self> {
        let tasks = parse_scheduler_tasks(config.raw())?;
        Ok(Self::new(tasks, queue))
    }

    pub fn from_config_with_executor(
        config: &Config,
        _queue: nzbg_queue::QueueHandle,
        executor: CommandExecutor,
    ) -> anyhow::Result<Self> {
        let tasks = parse_scheduler_tasks(config.raw())?;
        Ok(Self {
            tasks,
            executor,
            clock: Box::new(SystemClock),
        })
    }

    pub fn with_executor(mut self, executor: CommandExecutor) -> Self {
        self.executor = executor;
        self
    }

    pub fn with_clock(mut self, clock: Box<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    async fn tick_with_time(&mut self, now: NaiveDateTime) -> anyhow::Result<()> {
        for task in &mut self.tasks {
            if task.should_execute(now) {
                tracing::info!(
                    "executing scheduled task {}: {:?} {}",
                    task.id,
                    task.command,
                    task.param
                );
                self.executor.execute(&task.command, &task.param).await?;
                task.last_executed = Some(now);
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Service for Scheduler {
    fn name(&self) -> &str {
        "Scheduler"
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(30)
    }

    async fn tick(&mut self) -> anyhow::Result<()> {
        self.tick_with_time(self.clock.now()).await
    }
}

#[derive(Clone, Default)]
pub struct CommandDeps {
    pub postproc_paused: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub scan_paused: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub scan_trigger: Option<tokio::sync::mpsc::Sender<()>>,
    pub feed_handle: Option<nzbg_feed::FeedHandle>,
    pub server_pool: Option<std::sync::Arc<nzbg_nntp::ServerPoolManager>>,
}

#[derive(Clone)]
pub struct CommandExecutor {
    queue: nzbg_queue::QueueHandle,
    deps: CommandDeps,
}

impl CommandExecutor {
    pub fn new(queue: nzbg_queue::QueueHandle) -> Self {
        Self {
            queue,
            deps: CommandDeps::default(),
        }
    }

    pub fn with_deps(mut self, deps: CommandDeps) -> Self {
        self.deps = deps;
        self
    }

    pub async fn execute(&self, command: &SchedulerCommand, param: &str) -> anyhow::Result<()> {
        match command {
            SchedulerCommand::PauseDownload => {
                self.queue.pause_all().await?;
            }
            SchedulerCommand::UnpauseDownload => {
                self.queue.resume_all().await?;
            }
            SchedulerCommand::DownloadRate => {
                let rate_kb: u64 = param.parse().unwrap_or(0);
                self.queue.set_download_rate(rate_kb * 1024).await?;
            }
            SchedulerCommand::ActivateServer => {
                let server_id: u32 = param.parse().context("activate server id")?;
                if let Some(pool) = &self.deps.server_pool {
                    if pool.activate_server(server_id).await {
                        tracing::info!("activated server {server_id}");
                    } else {
                        tracing::warn!("server {server_id} not found");
                    }
                }
            }
            SchedulerCommand::DeactivateServer => {
                let server_id: u32 = param.parse().context("deactivate server id")?;
                if let Some(pool) = &self.deps.server_pool {
                    if pool.deactivate_server(server_id).await {
                        tracing::info!("deactivated server {server_id}");
                    } else {
                        tracing::warn!("server {server_id} not found");
                    }
                }
            }
            SchedulerCommand::PausePostProcess => {
                if let Some(flag) = &self.deps.postproc_paused {
                    flag.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            SchedulerCommand::UnpausePostProcess => {
                if let Some(flag) = &self.deps.postproc_paused {
                    flag.store(false, std::sync::atomic::Ordering::Relaxed);
                }
            }
            SchedulerCommand::PauseScan => {
                if let Some(flag) = &self.deps.scan_paused {
                    flag.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            SchedulerCommand::UnpauseScan => {
                if let Some(flag) = &self.deps.scan_paused {
                    flag.store(false, std::sync::atomic::Ordering::Relaxed);
                }
            }
            SchedulerCommand::Process => {
                if let Some(trigger) = &self.deps.scan_trigger {
                    let _ = trigger.send(()).await;
                }
            }
            SchedulerCommand::FetchFeed => {
                let feed_id: u32 = param.parse().context("fetch feed id")?;
                if let Some(feed_handle) = &self.deps.feed_handle {
                    match feed_handle.process_feed(feed_id).await {
                        Ok(items) => {
                            tracing::info!("feed {feed_id}: fetched {} items", items.len());
                        }
                        Err(err) => {
                            tracing::warn!("feed {feed_id} fetch error: {err}");
                        }
                    }
                } else {
                    tracing::warn!("feed coordinator not available");
                }
            }
            SchedulerCommand::Extensions => {
                tracing::info!("extensions command: {param}");
            }
        }
        Ok(())
    }
}

pub trait Clock: Send + Sync {
    fn now(&self) -> NaiveDateTime;
}

#[derive(Debug, Clone)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> NaiveDateTime {
        chrono::Local::now().naive_local()
    }
}

pub struct NzbDirScanner {
    nzb_dir: PathBuf,
    file_age: Duration,
    interval: Duration,
    queue: Option<nzbg_queue::QueueHandle>,
    disk: Option<std::sync::Arc<nzbg_diskstate::DiskState<nzbg_diskstate::JsonFormat>>>,
}

impl NzbDirScanner {
    pub fn from_config(config: &Config) -> Self {
        let file_age = parse_seconds_option(config.raw(), "NzbDirFileAge", 60);
        let interval = parse_seconds_option(config.raw(), "NzbDirInterval", 5);
        Self {
            nzb_dir: config.nzb_dir.clone(),
            file_age: Duration::from_secs(file_age),
            interval: Duration::from_secs(interval),
            queue: None,
            disk: None,
        }
    }

    pub fn with_queue(mut self, queue: nzbg_queue::QueueHandle) -> Self {
        self.queue = Some(queue);
        self
    }

    pub fn with_disk(
        mut self,
        disk: std::sync::Arc<nzbg_diskstate::DiskState<nzbg_diskstate::JsonFormat>>,
    ) -> Self {
        self.disk = Some(disk);
        self
    }

    async fn process_nzb(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(queue) = &self.queue {
            let nzb_bytes = tokio::fs::read(path).await?;

            let category = path.parent().and_then(|p| {
                if p == self.nzb_dir {
                    None
                } else {
                    p.file_name().map(|n| n.to_string_lossy().to_string())
                }
            });
            let id = queue
                .add_nzb(
                    path.to_path_buf(),
                    category,
                    nzbg_core::models::Priority::Normal,
                )
                .await?;
            tracing::info!("added NZB {} as id {}", path.display(), id);

            if let Some(disk) = &self.disk
                && let Err(err) = disk.save_nzb_file(id, &nzb_bytes)
            {
                tracing::warn!("failed to save NZB file for id {id}: {err}");
            }

            let processed_path = path.with_extension("nzb.queued");
            if let Err(err) = tokio::fs::rename(path, &processed_path).await {
                tracing::warn!("failed to rename processed NZB {}: {}", path.display(), err);
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Service for NzbDirScanner {
    fn name(&self) -> &str {
        "NzbDirScanner"
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    async fn tick(&mut self) -> anyhow::Result<()> {
        let mut entries = tokio::fs::read_dir(&self.nzb_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "nzb") {
                let metadata = entry.metadata().await?;
                let modified = metadata.modified()?;
                let age = modified.elapsed().unwrap_or_default();

                if age >= self.file_age {
                    tracing::info!("processing NZB: {}", path.display());
                    self.process_nzb(&path).await?;
                }
            }
        }

        Ok(())
    }
}

pub async fn fetch_nzb_url(
    url: &str,
    dest_dir: &Path,
    max_retries: u32,
) -> anyhow::Result<PathBuf> {
    let filename = url.rsplit('/').next().unwrap_or("download.nzb").to_string();
    let dest_path = dest_dir.join(&filename);
    let temp_path = dest_dir.join(format!("{filename}.tmp"));

    let mut attempt = 0;
    let mut last_error = None;
    while attempt <= max_retries {
        if attempt > 0 {
            let delay = Duration::from_millis(250 * (1 << attempt.min(4)));
            tokio::time::sleep(delay).await;
        }
        attempt += 1;

        match fetch_url_once(url).await {
            Ok(data) => {
                tokio::fs::write(&temp_path, &data).await?;
                tokio::fs::rename(&temp_path, &dest_path).await?;
                tracing::info!("fetched NZB from {} -> {}", url, dest_path.display());
                return Ok(dest_path);
            }
            Err(err) => {
                tracing::warn!("fetch attempt {attempt} for {url} failed: {err}");
                last_error = Some(err);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("fetch failed with no error")))
}

async fn fetch_url_once(url: &str) -> anyhow::Result<Vec<u8>> {
    let resp = reqwest::get(url).await?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("HTTP {status} for {url}");
    }
    let bytes = resp.bytes().await?;
    Ok(bytes.to_vec())
}

type SpaceChecker = Box<dyn Fn(&Path) -> anyhow::Result<u64> + Send + Sync>;

pub struct DiskSpaceMonitor {
    paths: Vec<PathBuf>,
    threshold_mb: u64,
    paused_by_us: bool,
    queue: Option<nzbg_queue::QueueHandle>,
    check_space: Option<SpaceChecker>,
}

impl DiskSpaceMonitor {
    pub fn from_config(config: &Config) -> Self {
        let threshold_mb = config.disk_space as u64;
        let paths = vec![
            config.dest_dir.clone(),
            config.inter_dir.clone(),
            config.temp_dir.clone(),
        ];
        Self {
            paths,
            threshold_mb,
            paused_by_us: false,
            queue: None,
            check_space: None,
        }
    }

    pub fn with_queue(mut self, queue: nzbg_queue::QueueHandle) -> Self {
        self.queue = Some(queue);
        self
    }

    fn available_space_mb(&self, path: &Path) -> anyhow::Result<u64> {
        if let Some(checker) = &self.check_space {
            return checker(path);
        }
        let available = fs2::available_space(path)
            .with_context(|| format!("check available space for {}", path.display()))?;
        Ok(available / (1024 * 1024))
    }
}

#[async_trait]
impl Service for DiskSpaceMonitor {
    fn name(&self) -> &str {
        "DiskSpaceMonitor"
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(10)
    }

    async fn tick(&mut self) -> anyhow::Result<()> {
        if self.threshold_mb == 0 {
            return Ok(());
        }
        for path in &self.paths {
            let available = self.available_space_mb(path)?;
            if available < self.threshold_mb {
                if !self.paused_by_us {
                    tracing::warn!(
                        "disk space low on {}: {available} MB < {} MB threshold",
                        path.display(),
                        self.threshold_mb
                    );
                    if let Some(queue) = &self.queue {
                        queue.pause_all().await?;
                    }
                    self.paused_by_us = true;
                }
                return Ok(());
            }
        }

        if self.paused_by_us {
            tracing::info!("disk space recovered, resuming downloads");
            if let Some(queue) = &self.queue {
                queue.resume_all().await?;
            }
            self.paused_by_us = false;
        }

        Ok(())
    }
}

pub struct ConnectionCleanup {
    pool: Option<std::sync::Arc<nzbg_nntp::ServerPoolManager>>,
    idle_timeout: Duration,
}

impl ConnectionCleanup {
    pub fn new() -> Self {
        Self {
            pool: None,
            idle_timeout: Duration::from_secs(60),
        }
    }

    pub fn with_pool(mut self, pool: std::sync::Arc<nzbg_nntp::ServerPoolManager>) -> Self {
        self.pool = Some(pool);
        self
    }
}

impl Default for ConnectionCleanup {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Service for ConnectionCleanup {
    fn name(&self) -> &str {
        "ConnectionCleanup"
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(30)
    }

    async fn tick(&mut self) -> anyhow::Result<()> {
        if let Some(pool) = &self.pool {
            let closed = pool.cleanup_idle_connections(self.idle_timeout).await;
            if closed > 0 {
                tracing::info!("cleaned up {closed} idle connection(s)");
            }
        }
        Ok(())
    }
}

pub struct HistoryCleanup {
    keep_days: u32,
    queue: Option<nzbg_queue::QueueHandle>,
}

impl HistoryCleanup {
    pub fn from_config(config: &Config) -> Self {
        Self {
            keep_days: config.keep_history,
            queue: None,
        }
    }

    pub fn with_queue(mut self, queue: nzbg_queue::QueueHandle) -> Self {
        self.queue = Some(queue);
        self
    }
}

#[async_trait]
impl Service for HistoryCleanup {
    fn name(&self) -> &str {
        "HistoryCleanup"
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(3600)
    }

    async fn tick(&mut self) -> anyhow::Result<()> {
        if self.keep_days == 0 {
            return Ok(());
        }
        let Some(queue) = &self.queue else {
            return Ok(());
        };
        let cutoff = std::time::SystemTime::now()
            - std::time::Duration::from_secs(self.keep_days as u64 * 86400);
        let history = queue.get_history().await?;
        for entry in &history {
            if entry.time < cutoff {
                tracing::info!("cleaning up history entry {}: {}", entry.id, entry.name);
                if let Err(err) = queue.history_delete(entry.id).await {
                    tracing::warn!("failed to delete history entry {}: {err}", entry.id);
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct ServerVolume {
    pub bytes_today: u64,
    pub bytes_this_month: u64,
    pub daily_history: Vec<(NaiveDate, u64)>,
    pub monthly_history: Vec<(NaiveDate, u64)>,
}

pub struct StatsTracker {
    volumes: std::collections::HashMap<u32, ServerVolume>,
    last_day: NaiveDate,
    quota_start_day: u32,
}

impl StatsTracker {
    pub fn from_config(config: &Config) -> Self {
        let quota_start_day = config
            .raw()
            .get("QuotaStartDay")
            .and_then(|value| value.parse().ok())
            .unwrap_or(1);
        let today = chrono::Local::now().date_naive();
        Self {
            volumes: std::collections::HashMap::new(),
            last_day: today,
            quota_start_day,
        }
    }

    pub fn record_bytes(&mut self, server_id: u32, bytes: u64) {
        let volume = self.volumes.entry(server_id).or_default();
        volume.bytes_today += bytes;
        volume.bytes_this_month += bytes;
    }

    pub fn check_monthly_quota(&self, server_id: u32, quota_gb: u64) -> bool {
        if quota_gb == 0 {
            return false;
        }
        let quota_bytes = quota_gb * 1024 * 1024 * 1024;
        self.volumes
            .get(&server_id)
            .is_some_and(|volume| volume.bytes_this_month >= quota_bytes)
    }

    fn roll_day(&mut self, today: NaiveDate) {
        for volume in self.volumes.values_mut() {
            volume
                .daily_history
                .push((self.last_day, volume.bytes_today));
            volume.bytes_today = 0;

            if today.day() == self.quota_start_day
                || (self.quota_start_day > 28
                    && today.day() == 1
                    && self.last_day.month() != today.month())
            {
                volume
                    .monthly_history
                    .push((self.last_day, volume.bytes_this_month));
                volume.bytes_this_month = 0;
            }
        }
        self.last_day = today;
    }
}

#[async_trait]
impl Service for StatsTracker {
    fn name(&self) -> &str {
        "StatsTracker"
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(60)
    }

    async fn tick(&mut self) -> anyhow::Result<()> {
        let today = chrono::Local::now().date_naive();
        if today != self.last_day {
            self.roll_day(today);
        }
        Ok(())
    }
}

pub struct SharedStatsTracker {
    inner: std::sync::Mutex<StatsTracker>,
}

impl SharedStatsTracker {
    pub fn new(tracker: StatsTracker) -> Self {
        Self {
            inner: std::sync::Mutex::new(tracker),
        }
    }
}

impl nzbg_nntp::StatsRecorder for SharedStatsTracker {
    fn record_bytes(&self, server_id: u32, bytes: u64) {
        if let Ok(mut tracker) = self.inner.lock() {
            tracker.record_bytes(server_id, bytes);
        }
    }
}

struct ServiceSharedStats(std::sync::Arc<SharedStatsTracker>);

#[async_trait]
impl Service for ServiceSharedStats {
    fn name(&self) -> &str {
        "StatsTracker"
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(60)
    }

    async fn tick(&mut self) -> anyhow::Result<()> {
        let today = chrono::Local::now().date_naive();
        if let Ok(mut tracker) = self.0.inner.lock()
            && today != tracker.last_day
        {
            tracker.roll_day(today);
        }
        Ok(())
    }
}

pub struct HealthChecker {
    dest_dir: PathBuf,
    cert_path: Option<PathBuf>,
    disk: Option<std::sync::Arc<nzbg_diskstate::DiskState<nzbg_diskstate::JsonFormat>>>,
    queue: Option<nzbg_queue::QueueHandle>,
}

impl HealthChecker {
    pub fn from_config(config: &Config) -> Self {
        Self {
            dest_dir: config.dest_dir.clone(),
            cert_path: config.secure_cert.clone(),
            disk: None,
            queue: None,
        }
    }

    pub fn with_disk(
        mut self,
        disk: std::sync::Arc<nzbg_diskstate::DiskState<nzbg_diskstate::JsonFormat>>,
    ) -> Self {
        self.disk = Some(disk);
        self
    }

    pub fn with_queue(mut self, queue: nzbg_queue::QueueHandle) -> Self {
        self.queue = Some(queue);
        self
    }

    async fn check_disk_write_speed(&self) -> anyhow::Result<()> {
        let test_path = self.dest_dir.join(".nzbg_health_check");
        let data = vec![0u8; 1024 * 1024];
        let start = std::time::Instant::now();
        tokio::fs::write(&test_path, &data).await?;
        tokio::fs::remove_file(&test_path).await?;
        let elapsed = start.elapsed();
        let speed_mb_s = 1.0 / elapsed.as_secs_f64();

        if speed_mb_s < 10.0 {
            tracing::warn!(
                "slow disk write speed: {speed_mb_s:.1} MB/s on {}",
                self.dest_dir.display()
            );
        }

        Ok(())
    }

    async fn check_certificate_expiry(&self) -> anyhow::Result<()> {
        let Some(cert_path) = &self.cert_path else {
            return Ok(());
        };
        match tokio::fs::metadata(cert_path).await {
            Ok(_) => {
                tracing::debug!("TLS certificate file exists: {}", cert_path.display());
            }
            Err(err) => {
                tracing::warn!(
                    "TLS certificate file not accessible: {}: {err}",
                    cert_path.display()
                );
            }
        }
        Ok(())
    }

    async fn check_queue_consistency(&self) -> anyhow::Result<()> {
        let (Some(disk), Some(queue)) = (&self.disk, &self.queue) else {
            return Ok(());
        };
        let snapshot = queue
            .get_queue_snapshot()
            .await
            .map_err(|e| anyhow::anyhow!("get queue snapshot: {e}"))?;
        let state = DiskStateFlush::snapshot_to_queue_state(&snapshot);
        let disk_clone = disk.clone();
        let warnings =
            tokio::task::spawn_blocking(move || disk_clone.validate_consistency(&state)).await??;
        for warning in &warnings {
            tracing::warn!("queue consistency: {warning:?}");
        }
        Ok(())
    }
}

#[async_trait]
impl Service for HealthChecker {
    fn name(&self) -> &str {
        "HealthChecker"
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(300)
    }

    async fn tick(&mut self) -> anyhow::Result<()> {
        self.check_disk_write_speed().await?;
        self.check_certificate_expiry().await?;
        self.check_queue_consistency().await?;
        Ok(())
    }
}

pub struct DiskStateFlush {
    queue: nzbg_queue::QueueHandle,
    disk: std::sync::Arc<nzbg_diskstate::DiskState<nzbg_diskstate::JsonFormat>>,
}

impl DiskStateFlush {
    pub fn new(
        queue: nzbg_queue::QueueHandle,
        disk: std::sync::Arc<nzbg_diskstate::DiskState<nzbg_diskstate::JsonFormat>>,
    ) -> Self {
        Self { queue, disk }
    }

    pub fn snapshot_to_queue_state(
        snapshot: &nzbg_queue::QueueSnapshot,
    ) -> nzbg_diskstate::QueueState {
        nzbg_diskstate::QueueState {
            version: 3,
            nzbs: snapshot
                .nzbs
                .iter()
                .map(|nzb| nzbg_diskstate::NzbState {
                    id: nzb.id,
                    name: nzb.name.clone(),
                    filename: nzb.filename.clone(),
                    category: nzb.category.clone(),
                    dest_dir: nzb.dest_dir.clone(),
                    final_dir: nzb.final_dir.clone(),
                    priority: nzb.priority as i32,
                    paused: nzb.paused,
                    url: if nzb.url.is_empty() {
                        None
                    } else {
                        Some(nzb.url.clone())
                    },
                    dupe_key: nzb.dupe_key.clone(),
                    dupe_score: nzb.dupe_score,
                    dupe_mode: nzb.dupe_mode,
                    added_time: chrono::DateTime::<chrono::Utc>::from(nzb.added_time),
                    total_size: nzb.total_size,
                    downloaded_size: nzb.downloaded_size,
                    failed_size: nzb.failed_size,
                    file_ids: nzb.file_ids.clone(),
                    post_process_parameters: nzb
                        .parameters
                        .iter()
                        .cloned()
                        .collect::<std::collections::HashMap<_, _>>(),
                    health: nzb.health,
                    critical_health: nzb.critical_health,
                    total_article_count: nzb.total_article_count,
                    success_article_count: nzb.success_article_count,
                    failed_article_count: nzb.failed_article_count,
                })
                .collect(),
            next_nzb_id: snapshot.next_nzb_id,
            next_file_id: snapshot.next_file_id,
            download_paused: snapshot.download_paused,
            speed_limit: snapshot.speed_limit,
        }
    }
}

#[async_trait]
impl Service for DiskStateFlush {
    fn name(&self) -> &str {
        "DiskStateFlush"
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(5)
    }

    async fn tick(&mut self) -> anyhow::Result<()> {
        let snapshot = self
            .queue
            .get_queue_snapshot()
            .await
            .map_err(|e| anyhow::anyhow!("failed to get queue snapshot: {e}"))?;

        let article_states = self
            .queue
            .get_all_file_article_states()
            .await
            .map_err(|e| anyhow::anyhow!("failed to get file article states: {e}"))?;

        let active_file_ids: std::collections::HashSet<u32> = snapshot
            .nzbs
            .iter()
            .flat_map(|nzb| nzb.file_ids.iter().copied())
            .collect();

        let state = Self::snapshot_to_queue_state(&snapshot);
        let disk = self.disk.clone();
        tokio::task::spawn_blocking(move || {
            disk.save_queue(&state)?;

            for snap in &article_states {
                let mut file_state =
                    nzbg_diskstate::FileArticleState::new(snap.file_id, snap.total_articles);
                for &(idx, crc) in &snap.completed_articles {
                    file_state.mark_article_done(idx, crc);
                }
                disk.save_file_state(snap.file_id, &file_state)?;
            }

            if let Ok(entries) = std::fs::read_dir(disk.state_dir().join("file")) {
                for entry in entries.flatten() {
                    if let Some(name) = entry.file_name().to_str()
                        && let Some(id_str) = name.strip_suffix(".state")
                        && let Ok(file_id) = id_str.parse::<u32>()
                        && !active_file_ids.contains(&file_id)
                    {
                        let _ = disk.delete_file_state(file_id);
                    }
                }
            }

            Ok::<(), anyhow::Error>(())
        })
        .await??;
        Ok(())
    }
}

pub fn parse_weekdays(value: &str) -> anyhow::Result<u8> {
    let mut mask = 0u8;
    for part in value.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((start, end)) = part.split_once('-') {
            let start = parse_weekday_number(start)?;
            let end = parse_weekday_number(end)?;
            if start > end {
                anyhow::bail!("weekday range must be ascending: {part}");
            }
            for day in start..=end {
                mask |= 1u8 << (day - 1);
            }
        } else {
            let day = parse_weekday_number(part)?;
            mask |= 1u8 << (day - 1);
        }
    }
    Ok(mask)
}

fn parse_weekday_number(value: &str) -> anyhow::Result<u8> {
    let day: u8 = value.trim().parse().context("weekday number")?;
    if !(1..=7).contains(&day) {
        anyhow::bail!("weekday must be in 1..=7: {day}");
    }
    Ok(day)
}

pub fn parse_scheduler_tasks(
    raw: &std::collections::HashMap<String, String>,
) -> anyhow::Result<Vec<ScheduledTask>> {
    let mut tasks = Vec::new();
    let mut index = 1u32;

    loop {
        let prefix = format!("Task{index}.");
        let time_key = format!("{prefix}Time");
        if !raw.contains_key(&time_key) {
            break;
        }

        let time_str = raw.get(&time_key).cloned().unwrap_or_default();
        let weekdays_str = raw
            .get(&format!("{prefix}WeekDays"))
            .cloned()
            .unwrap_or_else(|| "1-7".to_string());
        let command_str = raw
            .get(&format!("{prefix}Command"))
            .cloned()
            .unwrap_or_default();
        let param = raw
            .get(&format!("{prefix}Param"))
            .cloned()
            .unwrap_or_default();

        let time = NaiveTime::parse_from_str(&time_str, "%H:%M")
            .with_context(|| format!("parse {time_key}"))?;
        let weekdays =
            parse_weekdays(&weekdays_str).with_context(|| format!("parse {prefix}WeekDays"))?;
        let command = parse_scheduler_command(&command_str)
            .with_context(|| format!("parse {prefix}Command"))?;

        tasks.push(ScheduledTask {
            id: index,
            time,
            weekdays,
            command,
            param,
            last_executed: None,
        });

        index += 1;
    }

    Ok(tasks)
}

fn parse_scheduler_command(value: &str) -> anyhow::Result<SchedulerCommand> {
    match value.trim() {
        "PauseDownload" => Ok(SchedulerCommand::PauseDownload),
        "UnpauseDownload" => Ok(SchedulerCommand::UnpauseDownload),
        "PausePostProcess" => Ok(SchedulerCommand::PausePostProcess),
        "UnpausePostProcess" => Ok(SchedulerCommand::UnpausePostProcess),
        "DownloadRate" => Ok(SchedulerCommand::DownloadRate),
        "Extensions" => Ok(SchedulerCommand::Extensions),
        "Process" => Ok(SchedulerCommand::Process),
        "PauseScan" => Ok(SchedulerCommand::PauseScan),
        "UnpauseScan" => Ok(SchedulerCommand::UnpauseScan),
        "ActivateServer" => Ok(SchedulerCommand::ActivateServer),
        "DeactivateServer" => Ok(SchedulerCommand::DeactivateServer),
        "FetchFeed" => Ok(SchedulerCommand::FetchFeed),
        other => anyhow::bail!("unknown scheduler command: {other}"),
    }
}

fn parse_seconds_option(
    raw: &std::collections::HashMap<String, String>,
    key: &str,
    default_value: u64,
) -> u64 {
    raw.get(key)
        .and_then(|value| value.parse().ok())
        .unwrap_or(default_value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nzbg_core::models::Priority;
    use std::collections::HashMap;

    const SAMPLE_NZB: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
  <file poster="user@example.com" date="1706140800"
        subject='Test [01/01] - "data.rar" yEnc (1/1)'>
    <groups><group>alt.binaries.test</group></groups>
    <segments>
      <segment bytes="100" number="1">seg1@example.com</segment>
    </segments>
  </file>
</nzb>"#;

    struct FixedClock {
        now: NaiveDateTime,
    }

    impl Clock for FixedClock {
        fn now(&self) -> NaiveDateTime {
            self.now
        }
    }

    fn sample_task() -> ScheduledTask {
        ScheduledTask {
            id: 1,
            time: NaiveTime::from_hms_opt(3, 0, 0).unwrap(),
            weekdays: 0x7f,
            command: SchedulerCommand::PauseDownload,
            param: String::new(),
            last_executed: None,
        }
    }

    #[test]
    fn should_execute_requires_weekday_match() {
        let task = ScheduledTask {
            weekdays: 0x01,
            ..sample_task()
        };
        let now = NaiveDate::from_ymd_opt(2024, 1, 7)
            .unwrap()
            .and_hms_opt(3, 0, 0)
            .unwrap();
        assert!(!task.should_execute(now));
    }

    #[test]
    fn should_execute_requires_time_match() {
        let task = sample_task();
        let now = NaiveDate::from_ymd_opt(2024, 1, 1)
            .unwrap()
            .and_hms_opt(4, 0, 0)
            .unwrap();
        assert!(!task.should_execute(now));
    }

    #[test]
    fn should_execute_skips_duplicate_minute() {
        let mut task = sample_task();
        let now = NaiveDate::from_ymd_opt(2024, 1, 1)
            .unwrap()
            .and_hms_opt(3, 0, 0)
            .unwrap();
        task.last_executed = Some(now);
        assert!(!task.should_execute(now));
    }

    #[test]
    fn should_execute_accepts_match() {
        let task = sample_task();
        let now = NaiveDate::from_ymd_opt(2024, 1, 2)
            .unwrap()
            .and_hms_opt(3, 0, 0)
            .unwrap();
        assert!(task.should_execute(now));
    }

    #[tokio::test]
    async fn scheduler_tick_updates_last_executed() {
        let now = NaiveDate::from_ymd_opt(2024, 1, 3)
            .unwrap()
            .and_hms_opt(3, 0, 0)
            .unwrap();
        let task = sample_task();
        let clock = FixedClock { now };
        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });
        let mut scheduler = Scheduler::new(vec![task], handle.clone()).with_clock(Box::new(clock));

        scheduler.tick_with_time(now).await.expect("tick");

        assert_eq!(scheduler.tasks[0].last_executed, Some(now));
        handle.shutdown().await.expect("shutdown");
    }

    #[test]
    fn parse_weekdays_supports_ranges_and_lists() {
        let mask = parse_weekdays("1-5,7").expect("parse weekdays");
        assert_eq!(mask, 0x5f);
    }

    #[test]
    fn parse_scheduler_tasks_reads_sequential_entries() {
        let mut raw = HashMap::new();
        raw.insert("Task1.Time".to_string(), "03:00".to_string());
        raw.insert("Task1.WeekDays".to_string(), "1-7".to_string());
        raw.insert("Task1.Command".to_string(), "PauseDownload".to_string());
        raw.insert("Task1.Param".to_string(), "".to_string());
        raw.insert("Task2.Time".to_string(), "04:15".to_string());
        raw.insert("Task2.WeekDays".to_string(), "1,3,5".to_string());
        raw.insert("Task2.Command".to_string(), "DownloadRate".to_string());
        raw.insert("Task2.Param".to_string(), "500".to_string());

        let tasks = parse_scheduler_tasks(&raw).expect("tasks");
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].id, 1);
        assert_eq!(tasks[1].id, 2);
        assert_eq!(tasks[1].param, "500");
        assert_eq!(tasks[1].command, SchedulerCommand::DownloadRate);
    }

    #[test]
    fn parse_scheduler_tasks_stops_at_gap() {
        let mut raw = HashMap::new();
        raw.insert("Task1.Time".to_string(), "03:00".to_string());
        raw.insert("Task1.Command".to_string(), "PauseDownload".to_string());
        raw.insert("Task3.Time".to_string(), "04:00".to_string());
        raw.insert("Task3.Command".to_string(), "PauseDownload".to_string());

        let tasks = parse_scheduler_tasks(&raw).expect("tasks");
        assert_eq!(tasks.len(), 1);
    }

    #[tokio::test]
    async fn command_executor_pause_download_pauses_queue() {
        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let executor = CommandExecutor::new(handle.clone());
        executor
            .execute(&SchedulerCommand::PauseDownload, "")
            .await
            .expect("execute");

        let status = handle.get_status().await.expect("status");
        assert!(status.paused);
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn command_executor_unpause_download_resumes_queue() {
        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let executor = CommandExecutor::new(handle.clone());
        executor
            .execute(&SchedulerCommand::PauseDownload, "")
            .await
            .expect("pause");
        executor
            .execute(&SchedulerCommand::UnpauseDownload, "")
            .await
            .expect("unpause");

        let status = handle.get_status().await.expect("status");
        assert!(!status.paused);
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn command_executor_download_rate_sets_rate() {
        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let executor = CommandExecutor::new(handle.clone());
        executor
            .execute(&SchedulerCommand::DownloadRate, "500")
            .await
            .expect("rate");

        let status = handle.get_status().await.expect("status");
        assert_eq!(status.download_rate, 500 * 1024);
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn disk_state_flush_saves_queue_snapshot() {
        use nzbg_diskstate::{DiskState, JsonFormat};

        let nzb_dir = tempfile::tempdir().expect("nzb tempdir");
        let nzb_path = nzb_dir.path().join("test.nzb");
        std::fs::write(&nzb_path, SAMPLE_NZB).expect("write nzb");

        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        handle
            .add_nzb(nzb_path, None, Priority::Normal)
            .await
            .expect("add");

        let tmp = tempfile::tempdir().expect("tempdir");
        let disk = std::sync::Arc::new(
            DiskState::new(tmp.path().to_path_buf(), JsonFormat).expect("disk"),
        );

        let mut flusher = DiskStateFlush::new(handle.clone(), disk.clone());
        flusher.tick().await.expect("tick");

        let loaded = disk.load_queue().expect("load");
        assert_eq!(loaded.nzbs.len(), 1);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn disk_state_flush_saves_file_article_states() {
        use nzbg_diskstate::{DiskState, JsonFormat};

        let nzb_dir = tempfile::tempdir().expect("nzb tempdir");
        let nzb_path = nzb_dir.path().join("test.nzb");
        std::fs::write(&nzb_path, SAMPLE_NZB).expect("write nzb");

        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        handle
            .add_nzb(nzb_path, None, Priority::Normal)
            .await
            .expect("add");

        let assignments = {
            let snapshot = handle.get_queue_snapshot().await.expect("snap");
            let file_id = snapshot.nzbs[0].file_ids[0];
            let nzb_id = snapshot.nzbs[0].id;
            (nzb_id, file_id)
        };

        handle
            .report_download(nzbg_queue::DownloadResult {
                article_id: nzbg_queue::ArticleId {
                    nzb_id: assignments.0,
                    file_idx: 0,
                    seg_idx: 0,
                },
                outcome: nzbg_queue::DownloadOutcome::Success {
                    data: vec![1, 2, 3],
                    offset: 0,
                    crc: 0xDEAD,
                },
            })
            .await
            .expect("report");

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let tmp = tempfile::tempdir().expect("tempdir");
        let disk = std::sync::Arc::new(
            DiskState::new(tmp.path().to_path_buf(), JsonFormat).expect("disk"),
        );

        let mut flusher = DiskStateFlush::new(handle.clone(), disk.clone());
        flusher.tick().await.expect("tick");

        let loaded = disk.load_queue().expect("load queue");
        assert_eq!(loaded.nzbs.len(), 0);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn disk_state_flush_cleans_orphaned_file_states() {
        use nzbg_diskstate::{DiskState, FileArticleState, JsonFormat};

        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let tmp = tempfile::tempdir().expect("tempdir");
        let disk = std::sync::Arc::new(
            DiskState::new(tmp.path().to_path_buf(), JsonFormat).expect("disk"),
        );

        let orphan_state = FileArticleState::new(9999, 5);
        disk.save_file_state(9999, &orphan_state)
            .expect("save orphan");
        assert!(disk.load_file_state(9999).is_ok());

        let mut flusher = DiskStateFlush::new(handle.clone(), disk.clone());
        flusher.tick().await.expect("tick");

        assert!(disk.load_file_state(9999).is_err());

        handle.shutdown().await.expect("shutdown");
    }

    #[test]
    fn snapshot_to_queue_state_maps_new_fields() {
        use std::time::{Duration, SystemTime};

        let added = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let snapshot = nzbg_queue::QueueSnapshot {
            nzbs: vec![nzbg_queue::NzbSnapshotEntry {
                id: 42,
                name: "test".to_string(),
                filename: "test.nzb".to_string(),
                url: "http://example.com/test.nzb".to_string(),
                category: "movies".to_string(),
                dest_dir: std::path::PathBuf::from("/dst"),
                final_dir: Some(std::path::PathBuf::from("/final")),
                priority: Priority::High,
                paused: true,
                dupe_key: "mykey".to_string(),
                dupe_score: 99,
                dupe_mode: nzbg_core::models::DupMode::All,
                added_time: added,
                total_size: 1000,
                downloaded_size: 500,
                failed_size: 10,
                health: 990,
                critical_health: 900,
                total_article_count: 100,
                success_article_count: 50,
                failed_article_count: 1,
                parameters: vec![
                    ("key1".to_string(), "val1".to_string()),
                    ("key2".to_string(), "val2".to_string()),
                ],
                file_ids: vec![1, 2, 3],
            }],
            history: vec![],
            next_nzb_id: 43,
            next_file_id: 10,
            download_paused: false,
            speed_limit: 0,
        };

        let state = DiskStateFlush::snapshot_to_queue_state(&snapshot);
        let nzb = &state.nzbs[0];

        assert_eq!(nzb.url.as_deref(), Some("http://example.com/test.nzb"));
        assert_eq!(nzb.dupe_key, "mykey");
        assert_eq!(nzb.dupe_score, 99);
        assert_eq!(nzb.dupe_mode, nzbg_core::models::DupMode::All);
        assert_eq!(nzb.added_time, chrono::DateTime::<chrono::Utc>::from(added));
        assert_eq!(
            nzb.final_dir.as_ref().map(|p| p.as_path()),
            Some(std::path::Path::new("/final"))
        );
        assert_eq!(nzb.post_process_parameters.len(), 2);
        assert_eq!(
            nzb.post_process_parameters.get("key1").map(|s| s.as_str()),
            Some("val1")
        );
        assert_eq!(
            nzb.post_process_parameters.get("key2").map(|s| s.as_str()),
            Some("val2")
        );
    }

    #[test]
    fn snapshot_to_queue_state_empty_url_maps_to_none() {
        let snapshot = nzbg_queue::QueueSnapshot {
            nzbs: vec![nzbg_queue::NzbSnapshotEntry {
                id: 1,
                name: "test".to_string(),
                filename: "test.nzb".to_string(),
                url: String::new(),
                category: String::new(),
                dest_dir: std::path::PathBuf::new(),
                final_dir: None,
                priority: Priority::Normal,
                paused: false,
                dupe_key: String::new(),
                dupe_score: 0,
                dupe_mode: nzbg_core::models::DupMode::Score,
                added_time: std::time::SystemTime::UNIX_EPOCH,
                total_size: 0,
                downloaded_size: 0,
                failed_size: 0,
                health: 1000,
                critical_health: 1000,
                total_article_count: 0,
                success_article_count: 0,
                failed_article_count: 0,
                parameters: vec![],
                file_ids: vec![],
            }],
            history: vec![],
            next_nzb_id: 2,
            next_file_id: 1,
            download_paused: false,
            speed_limit: 0,
        };

        let state = DiskStateFlush::snapshot_to_queue_state(&snapshot);
        assert!(state.nzbs[0].url.is_none());
        assert!(state.nzbs[0].final_dir.is_none());
        assert!(state.nzbs[0].post_process_parameters.is_empty());
    }

    #[tokio::test]
    async fn nzb_dir_scanner_processes_nzb_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let nzb_path = tmp.path().join("test.nzb");
        std::fs::write(&nzb_path, SAMPLE_NZB).expect("write");

        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let mut scanner = NzbDirScanner {
            nzb_dir: tmp.path().to_path_buf(),
            file_age: Duration::from_secs(0),
            interval: Duration::from_secs(5),
            queue: Some(handle.clone()),
            disk: None,
        };

        scanner.tick().await.expect("tick");

        let list = handle.get_nzb_list().await.expect("list");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "test.nzb");

        assert!(tmp.path().join("test.nzb.queued").exists());
        assert!(!nzb_path.exists());

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn nzb_dir_scanner_skips_non_nzb_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("readme.txt"), "text").expect("write");

        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let mut scanner = NzbDirScanner {
            nzb_dir: tmp.path().to_path_buf(),
            file_age: Duration::from_secs(0),
            interval: Duration::from_secs(5),
            queue: Some(handle.clone()),
            disk: None,
        };

        scanner.tick().await.expect("tick");

        let list = handle.get_nzb_list().await.expect("list");
        assert!(list.is_empty());

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn nzb_dir_scanner_idempotent_on_second_run() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("test.nzb"), SAMPLE_NZB).expect("write");

        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let mut scanner = NzbDirScanner {
            nzb_dir: tmp.path().to_path_buf(),
            file_age: Duration::from_secs(0),
            interval: Duration::from_secs(5),
            queue: Some(handle.clone()),
            disk: None,
        };

        scanner.tick().await.expect("tick1");
        scanner.tick().await.expect("tick2");

        let list = handle.get_nzb_list().await.expect("list");
        assert_eq!(list.len(), 1);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn command_executor_pause_postprocess_sets_flag() {
        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let deps = CommandDeps {
            postproc_paused: Some(flag.clone()),
            ..Default::default()
        };
        let executor = CommandExecutor::new(handle.clone()).with_deps(deps);
        executor
            .execute(&SchedulerCommand::PausePostProcess, "")
            .await
            .expect("execute");
        assert!(flag.load(std::sync::atomic::Ordering::Relaxed));
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn command_executor_unpause_postprocess_clears_flag() {
        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let deps = CommandDeps {
            postproc_paused: Some(flag.clone()),
            ..Default::default()
        };
        let executor = CommandExecutor::new(handle.clone()).with_deps(deps);
        executor
            .execute(&SchedulerCommand::UnpausePostProcess, "")
            .await
            .expect("execute");
        assert!(!flag.load(std::sync::atomic::Ordering::Relaxed));
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn command_executor_pause_scan_sets_flag() {
        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let deps = CommandDeps {
            scan_paused: Some(flag.clone()),
            ..Default::default()
        };
        let executor = CommandExecutor::new(handle.clone()).with_deps(deps);
        executor
            .execute(&SchedulerCommand::PauseScan, "")
            .await
            .expect("execute");
        assert!(flag.load(std::sync::atomic::Ordering::Relaxed));
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn command_executor_unpause_scan_clears_flag() {
        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let deps = CommandDeps {
            scan_paused: Some(flag.clone()),
            ..Default::default()
        };
        let executor = CommandExecutor::new(handle.clone()).with_deps(deps);
        executor
            .execute(&SchedulerCommand::UnpauseScan, "")
            .await
            .expect("execute");
        assert!(!flag.load(std::sync::atomic::Ordering::Relaxed));
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn command_executor_process_triggers_scan() {
        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let (trigger_tx, mut trigger_rx) = tokio::sync::mpsc::channel(4);
        let deps = CommandDeps {
            scan_trigger: Some(trigger_tx),
            ..Default::default()
        };
        let executor = CommandExecutor::new(handle.clone()).with_deps(deps);
        executor
            .execute(&SchedulerCommand::Process, "")
            .await
            .expect("execute");

        let received = trigger_rx.try_recv();
        assert!(received.is_ok());
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn command_executor_activate_server() {
        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let mut server = nzbg_nntp::NewsServer {
            id: 1,
            name: "test".to_string(),
            active: false,
            host: "localhost".to_string(),
            port: 119,
            username: None,
            password: None,
            encryption: nzbg_nntp::Encryption::None,
            cipher: None,
            connections: 2,
            retention: 0,
            level: 0,
            optional: false,
            group: 0,
            join_group: true,
            ip_version: nzbg_nntp::IpVersion::Auto,
            cert_verification: false,
        };
        let manager = std::sync::Arc::new(nzbg_nntp::ServerPoolManager::new(vec![server.clone()]));
        assert_eq!(manager.server_count().await, 0);

        let deps = CommandDeps {
            server_pool: Some(manager.clone()),
            ..Default::default()
        };
        let executor = CommandExecutor::new(handle.clone()).with_deps(deps);
        executor
            .execute(&SchedulerCommand::ActivateServer, "1")
            .await
            .expect("execute");
        assert_eq!(manager.server_count().await, 1);
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn command_executor_deactivate_server() {
        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let server = nzbg_nntp::NewsServer {
            id: 1,
            name: "test".to_string(),
            active: true,
            host: "localhost".to_string(),
            port: 119,
            username: None,
            password: None,
            encryption: nzbg_nntp::Encryption::None,
            cipher: None,
            connections: 2,
            retention: 0,
            level: 0,
            optional: false,
            group: 0,
            join_group: true,
            ip_version: nzbg_nntp::IpVersion::Auto,
            cert_verification: false,
        };
        let manager = std::sync::Arc::new(nzbg_nntp::ServerPoolManager::new(vec![server]));
        assert_eq!(manager.server_count().await, 1);

        let deps = CommandDeps {
            server_pool: Some(manager.clone()),
            ..Default::default()
        };
        let executor = CommandExecutor::new(handle.clone()).with_deps(deps);
        executor
            .execute(&SchedulerCommand::DeactivateServer, "1")
            .await
            .expect("execute");
        assert_eq!(manager.server_count().await, 0);
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn command_executor_fetch_feed_calls_handle() {
        use nzbg_feed::{FeedConfig, FeedCoordinator, FeedHandle, FeedHistoryDb};

        struct MockFetcher;
        #[async_trait::async_trait]
        impl nzbg_feed::fetch::FeedFetcher for MockFetcher {
            async fn fetch_url(&self, _url: &str) -> Result<Vec<u8>, anyhow::Error> {
                Ok(br#"<?xml version="1.0"?><rss><channel>
                    <item><title>Test</title><link>https://example.com/1.nzb</link></item>
                </channel></rss>"#
                    .to_vec())
            }
        }

        let feed = FeedConfig {
            id: 1,
            name: "Test".to_string(),
            url: "https://example.com/rss".to_string(),
            filter: String::new(),
            interval_min: 15,
            backlog: false,
            pause_nzb: false,
            category: "TV".to_string(),
            priority: 0,
            extensions: vec![],
        };
        let history = FeedHistoryDb::new(30);
        let coordinator = FeedCoordinator::new(vec![feed], history, Box::new(MockFetcher));
        let (feed_tx, feed_rx) = tokio::sync::mpsc::channel(4);
        let feed_handle = FeedHandle::new(feed_tx);
        tokio::spawn(coordinator.run_actor(feed_rx));

        let (mut queue_coord, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        tokio::spawn(async move { queue_coord.run().await });

        let deps = CommandDeps {
            feed_handle: Some(feed_handle),
            ..Default::default()
        };
        let executor = CommandExecutor::new(handle.clone()).with_deps(deps);
        let result = executor.execute(&SchedulerCommand::FetchFeed, "1").await;
        assert!(result.is_ok());
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn command_executor_extensions_succeeds() {
        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let executor = CommandExecutor::new(handle.clone());
        let result = executor
            .execute(&SchedulerCommand::Extensions, "some-script")
            .await;
        assert!(result.is_ok());
        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn disk_space_monitor_pauses_queue_on_low_space() {
        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut monitor = DiskSpaceMonitor {
            paths: vec![tmp.path().to_path_buf()],
            threshold_mb: u64::MAX,
            paused_by_us: false,
            queue: Some(handle.clone()),
            check_space: None,
        };

        monitor.tick().await.expect("tick");
        assert!(monitor.paused_by_us);

        let status = handle.get_status().await.expect("status");
        assert!(status.paused);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn disk_space_monitor_resumes_queue_on_recovery() {
        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut monitor = DiskSpaceMonitor {
            paths: vec![tmp.path().to_path_buf()],
            threshold_mb: 1,
            paused_by_us: true,
            queue: Some(handle.clone()),
            check_space: None,
        };

        handle.pause_all().await.expect("pre-pause");

        monitor.tick().await.expect("tick");
        assert!(!monitor.paused_by_us);

        let status = handle.get_status().await.expect("status");
        assert!(!status.paused);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn disk_space_monitor_no_pause_when_threshold_zero() {
        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut monitor = DiskSpaceMonitor {
            paths: vec![tmp.path().to_path_buf()],
            threshold_mb: 0,
            paused_by_us: false,
            queue: Some(handle.clone()),
            check_space: None,
        };

        monitor.tick().await.expect("tick");
        assert!(!monitor.paused_by_us);

        let status = handle.get_status().await.expect("status");
        assert!(!status.paused);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn disk_space_monitor_no_queue_still_works() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut monitor = DiskSpaceMonitor {
            paths: vec![tmp.path().to_path_buf()],
            threshold_mb: u64::MAX,
            paused_by_us: false,
            queue: None,
            check_space: None,
        };

        monitor.tick().await.expect("tick");
        assert!(monitor.paused_by_us);
    }

    #[tokio::test]
    async fn disk_space_monitor_custom_check_space() {
        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut monitor = DiskSpaceMonitor {
            paths: vec![tmp.path().to_path_buf()],
            threshold_mb: 100,
            paused_by_us: false,
            queue: Some(handle.clone()),
            check_space: Some(Box::new(|_path| Ok(50))),
        };

        monitor.tick().await.expect("tick");
        assert!(monitor.paused_by_us);

        let status = handle.get_status().await.expect("status");
        assert!(status.paused);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn disk_space_monitor_with_queue_builder() {
        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let config = nzbg_config::Config::from_raw(std::collections::HashMap::new());
        let monitor = DiskSpaceMonitor::from_config(&config).with_queue(handle.clone());
        assert!(monitor.queue.is_some());

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn connection_cleanup_without_pool_succeeds() {
        let mut cleanup = ConnectionCleanup::new();
        cleanup
            .tick()
            .await
            .expect("tick without pool should succeed");
    }

    #[tokio::test]
    async fn connection_cleanup_with_pool_succeeds() {
        let manager = std::sync::Arc::new(nzbg_nntp::ServerPoolManager::new(vec![]));
        let mut cleanup = ConnectionCleanup::new().with_pool(manager);
        cleanup.tick().await.expect("tick with pool should succeed");
    }

    fn sample_nzb(id: u32, name: &str) -> nzbg_core::models::NzbInfo {
        nzbg_core::models::NzbInfo {
            id,
            kind: nzbg_core::models::NzbKind::Nzb,
            name: name.to_string(),
            filename: name.to_string(),
            url: String::new(),
            dest_dir: std::path::PathBuf::new(),
            final_dir: std::path::PathBuf::new(),
            temp_dir: std::path::PathBuf::new(),
            queue_dir: std::path::PathBuf::new(),
            category: String::new(),
            priority: nzbg_core::models::Priority::Normal,
            dup_key: String::new(),
            dup_mode: nzbg_core::models::DupMode::Score,
            dup_score: 0,
            size: 0,
            remaining_size: 0,
            paused_size: 0,
            failed_size: 0,
            success_size: 0,
            current_downloaded_size: 0,
            par_size: 0,
            par_remaining_size: 0,
            par_current_success_size: 0,
            par_failed_size: 0,
            file_count: 0,
            remaining_file_count: 0,
            remaining_par_count: 0,
            total_article_count: 0,
            success_article_count: 0,
            failed_article_count: 0,
            added_time: std::time::SystemTime::UNIX_EPOCH,
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
            par_status: nzbg_core::models::ParStatus::None,
            unpack_status: nzbg_core::models::UnpackStatus::None,
            move_status: nzbg_core::models::MoveStatus::None,
            delete_status: nzbg_core::models::DeleteStatus::None,
            mark_status: nzbg_core::models::MarkStatus::None,
            url_status: nzbg_core::models::UrlStatus::None,
            health: 1000,
            critical_health: 1000,
            files: vec![],
            completed_files: vec![],
            server_stats: vec![],
            parameters: vec![],
            post_info: None,
            message_count: 0,
            cached_message_count: 0,
        }
    }

    #[tokio::test]
    async fn history_cleanup_skips_when_keep_days_zero() {
        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        coordinator.add_to_history(
            sample_nzb(1, "old-entry"),
            nzbg_core::models::HistoryKind::Nzb,
        );
        tokio::spawn(async move { coordinator.run().await });

        let mut cleanup = HistoryCleanup {
            keep_days: 0,
            queue: Some(handle.clone()),
        };
        cleanup.tick().await.expect("tick");

        let history = handle.get_history().await.expect("history");
        assert_eq!(
            history.len(),
            1,
            "entry should not be deleted when keep_days=0"
        );

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn history_cleanup_skips_when_no_queue() {
        let mut cleanup = HistoryCleanup {
            keep_days: 7,
            queue: None,
        };
        cleanup
            .tick()
            .await
            .expect("tick without queue should succeed");
    }

    #[tokio::test]
    async fn history_cleanup_preserves_fresh_entries() {
        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        coordinator.add_to_history(sample_nzb(1, "fresh"), nzbg_core::models::HistoryKind::Nzb);
        tokio::spawn(async move { coordinator.run().await });

        let mut cleanup = HistoryCleanup {
            keep_days: 1,
            queue: Some(handle.clone()),
        };
        cleanup.tick().await.expect("tick");

        let history = handle.get_history().await.expect("history");
        assert_eq!(history.len(), 1, "fresh entry should not be deleted");

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn history_cleanup_deletes_old_entries() {
        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);

        let queue = nzbg_core::models::DownloadQueue {
            queue: vec![],
            history: vec![nzbg_core::models::HistoryInfo {
                id: 1,
                kind: nzbg_core::models::HistoryKind::Nzb,
                time: std::time::SystemTime::UNIX_EPOCH,
                nzb_info: sample_nzb(1, "old-entry"),
            }],
            next_nzb_id: 3,
            next_file_id: 1,
        };
        coordinator.seed_state(queue, false, 0);
        coordinator.add_to_history(
            sample_nzb(2, "fresh-entry"),
            nzbg_core::models::HistoryKind::Nzb,
        );

        tokio::spawn(async move { coordinator.run().await });

        let mut cleanup = HistoryCleanup {
            keep_days: 1,
            queue: Some(handle.clone()),
        };
        cleanup.tick().await.expect("tick");

        let history = handle.get_history().await.expect("history");
        assert_eq!(history.len(), 1, "only old entry should be deleted");
        assert_eq!(history[0].name, "fresh-entry");

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn history_cleanup_deletes_multiple_old_entries() {
        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);

        let history_entries: Vec<_> = (1..=3)
            .map(|i| nzbg_core::models::HistoryInfo {
                id: i,
                kind: nzbg_core::models::HistoryKind::Nzb,
                time: std::time::SystemTime::UNIX_EPOCH,
                nzb_info: sample_nzb(i, &format!("old-{i}")),
            })
            .collect();
        let queue = nzbg_core::models::DownloadQueue {
            queue: vec![],
            history: history_entries,
            next_nzb_id: 4,
            next_file_id: 1,
        };
        coordinator.seed_state(queue, false, 0);

        tokio::spawn(async move { coordinator.run().await });

        let mut cleanup = HistoryCleanup {
            keep_days: 1,
            queue: Some(handle.clone()),
        };
        cleanup.tick().await.expect("tick");

        let history = handle.get_history().await.expect("history");
        assert!(history.is_empty(), "all old entries should be deleted");

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn health_checker_check_cert_no_path_skips() {
        let checker = HealthChecker {
            dest_dir: PathBuf::from("/tmp"),
            cert_path: None,
            disk: None,
            queue: None,
        };
        checker
            .check_certificate_expiry()
            .await
            .expect("should succeed when no cert configured");
    }

    #[tokio::test]
    async fn health_checker_check_cert_missing_file_doesnt_error() {
        let checker = HealthChecker {
            dest_dir: PathBuf::from("/tmp"),
            cert_path: Some(PathBuf::from("/nonexistent/cert.pem")),
            disk: None,
            queue: None,
        };
        checker
            .check_certificate_expiry()
            .await
            .expect("should succeed even when cert file is missing");
    }

    #[tokio::test]
    async fn health_checker_check_cert_existing_file_succeeds() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cert_file = tmp.path().join("test.pem");
        std::fs::write(&cert_file, b"fake cert data").expect("write");
        let checker = HealthChecker {
            dest_dir: PathBuf::from("/tmp"),
            cert_path: Some(cert_file),
            disk: None,
            queue: None,
        };
        checker
            .check_certificate_expiry()
            .await
            .expect("should succeed for existing cert file");
    }

    #[tokio::test]
    async fn health_checker_check_consistency_no_deps_skips() {
        let checker = HealthChecker {
            dest_dir: PathBuf::from("/tmp"),
            cert_path: None,
            disk: None,
            queue: None,
        };
        checker
            .check_queue_consistency()
            .await
            .expect("should succeed when no disk/queue configured");
    }

    #[tokio::test]
    async fn health_checker_check_consistency_empty_queue() {
        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let tmp = tempfile::tempdir().expect("tempdir");
        let disk = std::sync::Arc::new(
            nzbg_diskstate::DiskState::new(tmp.path().to_path_buf(), nzbg_diskstate::JsonFormat)
                .expect("disk state"),
        );

        let checker = HealthChecker {
            dest_dir: PathBuf::from("/tmp"),
            cert_path: None,
            disk: Some(disk),
            queue: Some(handle.clone()),
        };
        checker
            .check_queue_consistency()
            .await
            .expect("should succeed with empty queue");

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn health_checker_check_consistency_reports_warnings() {
        let nzb_info = sample_nzb(1, "test");
        let queue = nzbg_core::models::DownloadQueue {
            queue: vec![nzb_info],
            history: vec![],
            next_nzb_id: 2,
            next_file_id: 1,
        };

        let (mut coordinator, handle, _rx, _rate_rx) = nzbg_queue::QueueCoordinator::new(2, 1);
        coordinator.seed_state(queue, false, 0);
        tokio::spawn(async move { coordinator.run().await });

        let tmp = tempfile::tempdir().expect("tempdir");
        let disk = std::sync::Arc::new(
            nzbg_diskstate::DiskState::new(tmp.path().to_path_buf(), nzbg_diskstate::JsonFormat)
                .expect("disk state"),
        );

        let checker = HealthChecker {
            dest_dir: PathBuf::from("/tmp"),
            cert_path: None,
            disk: Some(disk),
            queue: Some(handle.clone()),
        };
        let result = checker.check_queue_consistency().await;
        assert!(result.is_ok(), "should succeed even with warnings");

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn connection_cleanup_logs_cleaned_count() {
        let server = nzbg_nntp::NewsServer {
            id: 1,
            name: "test".to_string(),
            active: true,
            host: "localhost".to_string(),
            port: 119,
            username: None,
            password: None,
            encryption: nzbg_nntp::Encryption::None,
            cipher: None,
            connections: 2,
            retention: 0,
            level: 0,
            optional: false,
            group: 0,
            join_group: true,
            ip_version: nzbg_nntp::IpVersion::Auto,
            cert_verification: false,
        };
        let manager = std::sync::Arc::new(nzbg_nntp::ServerPoolManager::new(vec![server]));
        let mut cleanup = ConnectionCleanup::new().with_pool(manager);
        let result = cleanup.tick().await;
        assert!(result.is_ok());
    }
}
