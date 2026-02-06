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

pub async fn start_services(
    config: &Config,
) -> anyhow::Result<(broadcast::Sender<()>, Vec<tokio::task::JoinHandle<()>>)> {
    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    let services = build_services(config)?;
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

fn build_services(config: &Config) -> anyhow::Result<Vec<Box<dyn Service>>> {
    let scheduler = Scheduler::from_config(config)?;
    let scanner = NzbDirScanner::from_config(config);
    let disk_space = DiskSpaceMonitor::from_config(config);
    let history = HistoryCleanup::from_config(config);
    let stats = StatsTracker::from_config(config);
    let health = HealthChecker::from_config(config);

    Ok(vec![
        Box::new(scheduler),
        Box::new(scanner),
        Box::new(disk_space),
        Box::new(ConnectionCleanup),
        Box::new(history),
        Box::new(stats),
        Box::new(health),
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
    pub fn new(tasks: Vec<ScheduledTask>) -> Self {
        Self {
            tasks,
            executor: CommandExecutor,
            clock: Box::new(SystemClock),
        }
    }

    pub fn from_config(config: &Config) -> anyhow::Result<Self> {
        let tasks = parse_scheduler_tasks(config.raw())?;
        Ok(Self::new(tasks))
    }

    pub fn with_executor(mut self, executor: CommandExecutor) -> Self {
        self.executor = executor;
        self
    }

    pub fn with_clock(mut self, clock: Box<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    fn tick_with_time(&mut self, now: NaiveDateTime) -> anyhow::Result<()> {
        for task in &mut self.tasks {
            if task.should_execute(now) {
                tracing::info!(
                    "executing scheduled task {}: {:?} {}",
                    task.id,
                    task.command,
                    task.param
                );
                self.executor.execute(&task.command, &task.param)?;
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
        self.tick_with_time(self.clock.now())
    }
}

#[derive(Debug, Clone, Default)]
pub struct CommandExecutor;

impl CommandExecutor {
    pub fn execute(&self, command: &SchedulerCommand, param: &str) -> anyhow::Result<()> {
        match command {
            SchedulerCommand::DownloadRate => {
                let _rate: u32 = param.parse().unwrap_or(0);
            }
            SchedulerCommand::ActivateServer => {
                let _server_id: u32 = param.parse().context("activate server id")?;
            }
            SchedulerCommand::DeactivateServer => {
                let _server_id: u32 = param.parse().context("deactivate server id")?;
            }
            SchedulerCommand::FetchFeed => {
                let _feed_id: u32 = param.parse().context("fetch feed id")?;
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
}

impl NzbDirScanner {
    pub fn from_config(config: &Config) -> Self {
        let file_age = parse_seconds_option(config.raw(), "NzbDirFileAge", 60);
        let interval = parse_seconds_option(config.raw(), "NzbDirInterval", 5);
        Self {
            nzb_dir: config.nzb_dir.clone(),
            file_age: Duration::from_secs(file_age),
            interval: Duration::from_secs(interval),
        }
    }

    async fn process_nzb(&self, _path: &Path) -> anyhow::Result<()> {
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
    use std::collections::HashMap;

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

    #[test]
    fn scheduler_tick_updates_last_executed() {
        let now = NaiveDate::from_ymd_opt(2024, 1, 3)
            .unwrap()
            .and_hms_opt(3, 0, 0)
            .unwrap();
        let task = sample_task();
        let clock = FixedClock { now };
        let mut scheduler = Scheduler::new(vec![task]).with_clock(Box::new(clock));

        scheduler.tick_with_time(now).expect("tick");

        assert_eq!(scheduler.tasks[0].last_executed, Some(now));
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
}
