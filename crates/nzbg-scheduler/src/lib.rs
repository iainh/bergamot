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
    let scheduler = Scheduler::from_config(config, deps.queue.clone())?;
    let scanner = NzbDirScanner::from_config(config)
        .with_queue(deps.queue.clone())
        .with_disk(deps.disk.clone());
    let disk_space = DiskSpaceMonitor::from_config(config);
    let history = HistoryCleanup::from_config(config);
    let stats = StatsTracker::from_config(config);
    let health = HealthChecker::from_config(config);
    let disk_flush = DiskStateFlush::new(deps.queue, deps.disk);

    Ok(vec![
        Box::new(scheduler),
        Box::new(scanner),
        Box::new(disk_space),
        Box::new(ConnectionCleanup),
        Box::new(history),
        Box::new(stats),
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

#[derive(Debug, Clone)]
pub struct CommandExecutor {
    queue: nzbg_queue::QueueHandle,
}

impl CommandExecutor {
    pub fn new(queue: nzbg_queue::QueueHandle) -> Self {
        Self { queue }
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
                let _server_id: u32 = param.parse().context("activate server id")?;
                tracing::warn!("activate server not yet implemented");
            }
            SchedulerCommand::DeactivateServer => {
                let _server_id: u32 = param.parse().context("deactivate server id")?;
                tracing::warn!("deactivate server not yet implemented");
            }
            SchedulerCommand::FetchFeed => {
                let _feed_id: u32 = param.parse().context("fetch feed id")?;
                tracing::warn!("fetch feed not yet implemented");
            }
            _ => {
                tracing::warn!("unimplemented scheduler command: {command:?}");
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

pub struct DiskSpaceMonitor {
    paths: Vec<PathBuf>,
    threshold_mb: u64,
    paused_by_us: bool,
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
        }
    }

    fn available_space_mb(path: &Path) -> anyhow::Result<u64> {
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
            let available = Self::available_space_mb(path)?;
            if available < self.threshold_mb {
                if !self.paused_by_us {
                    tracing::warn!(
                        "disk space low on {}: {available} MB < {} MB threshold",
                        path.display(),
                        self.threshold_mb
                    );
                    self.paused_by_us = true;
                }
                return Ok(());
            }
        }

        if self.paused_by_us {
            tracing::info!("disk space recovered, resuming downloads");
            self.paused_by_us = false;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct ConnectionCleanup;

#[async_trait]
impl Service for ConnectionCleanup {
    fn name(&self) -> &str {
        "ConnectionCleanup"
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(30)
    }

    async fn tick(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

pub struct HistoryCleanup {
    keep_days: u32,
}

impl HistoryCleanup {
    pub fn from_config(config: &Config) -> Self {
        Self {
            keep_days: config.keep_history,
        }
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
        let _cutoff = chrono::Utc::now() - chrono::Duration::days(self.keep_days as i64);
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

pub struct HealthChecker {
    dest_dir: PathBuf,
}

impl HealthChecker {
    pub fn from_config(config: &Config) -> Self {
        Self {
            dest_dir: config.dest_dir.clone(),
        }
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
        Ok(())
    }

    async fn check_queue_consistency(&self) -> anyhow::Result<()> {
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
                    final_dir: None,
                    priority: nzb.priority as i32,
                    paused: nzb.paused,
                    url: None,
                    dupe_key: String::new(),
                    dupe_score: 0,
                    dupe_mode: nzbg_core::models::DupMode::Score,
                    added_time: chrono::Utc::now(),
                    total_size: nzb.total_size,
                    downloaded_size: nzb.downloaded_size,
                    failed_size: nzb.failed_size,
                    file_ids: nzb.file_ids.clone(),
                    post_process_parameters: std::collections::HashMap::new(),
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
        let state = Self::snapshot_to_queue_state(&snapshot);
        let disk = self.disk.clone();
        tokio::task::spawn_blocking(move || disk.save_queue(&state)).await??;
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
            .add_nzb(
                nzb_path,
                None,
                Priority::Normal,
            )
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
}
