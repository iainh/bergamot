# Scheduler and Background Services

nzbg runs several background services alongside the download engine. Each service performs a periodic task — scanning for new NZBs, monitoring disk space, cleaning up connections, maintaining history, and executing scheduled commands.

## Service Trait

All background services implement a common trait:

```rust
use std::time::Duration;
use tokio::sync::broadcast;

#[async_trait::async_trait]
pub trait Service: Send + Sync + 'static {
    /// Human-readable name for logging.
    fn name(&self) -> &str;

    /// How often this service should tick.
    fn interval(&self) -> Duration;

    /// Perform one unit of work.
    async fn tick(&mut self) -> anyhow::Result<()>;
}
```

### Service Runner

Each service is spawned as a tokio task. The runner calls `tick()` at the configured interval and listens for a shutdown signal:

```rust
async fn run_service(
    mut service: Box<dyn Service>,
    mut shutdown: broadcast::Receiver<()>,
) {
    let name = service.name().to_string();
    tracing::info!("starting service: {name}");

    let mut interval = tokio::time::interval(service.interval());
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = interval.tick() => {
                if let Err(e) = service.tick().await {
                    tracing::error!("service {name} error: {e}");
                }
            }
            _ = shutdown.recv() => {
                tracing::info!("shutting down service: {name}");
                break;
            }
        }
    }
}
```

## Scheduler Service

The scheduler executes commands at configured times, replicating nzbget's `Task*` options.

### Data Model

```rust
use chrono::NaiveTime;

#[derive(Debug, Clone)]
pub struct ScheduledTask {
    pub id: u32,
    pub time: NaiveTime,
    /// Bitmask: bit 0 = Monday, bit 6 = Sunday. 0x7F = every day.
    pub weekdays: u8,
    pub command: SchedulerCommand,
    pub param: String,
    pub last_executed: Option<chrono::NaiveDateTime>,
}

#[derive(Debug, Clone, PartialEq)]
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
```

### Weekday Bitmask

```
Bit 0 = Monday     (0x01)
Bit 1 = Tuesday    (0x02)
Bit 2 = Wednesday  (0x04)
Bit 3 = Thursday   (0x08)
Bit 4 = Friday     (0x10)
Bit 5 = Saturday   (0x20)
Bit 6 = Sunday     (0x40)

0x7F = every day
0x1F = weekdays only
0x60 = weekends only
```

The config format `WeekDays=1-5` translates to `0x1F`. Individual days can be listed: `WeekDays=1,3,5` for Monday/Wednesday/Friday.

### Execution Logic

```rust
impl ScheduledTask {
    /// Check whether the task should execute at the given moment.
    pub fn should_execute(&self, now: chrono::NaiveDateTime) -> bool {
        // Check weekday (chrono: Mon=1 .. Sun=7, bitmask: Mon=bit 0)
        let weekday_bit = 1u8 << (now.weekday().num_days_from_monday());
        if self.weekdays & weekday_bit == 0 {
            return false;
        }

        // Check time (hour and minute match)
        if now.time().hour() != self.time.hour()
            || now.time().minute() != self.time.minute()
        {
            return false;
        }

        // Prevent duplicate execution within the same minute
        if let Some(last) = self.last_executed {
            if last.date() == now.date()
                && last.time().hour() == now.time().hour()
                && last.time().minute() == now.time().minute()
            {
                return false;
            }
        }

        true
    }
}
```

### Scheduler Tick

```rust
pub struct Scheduler {
    tasks: Vec<ScheduledTask>,
}

#[async_trait::async_trait]
impl Service for Scheduler {
    fn name(&self) -> &str { "Scheduler" }
    fn interval(&self) -> Duration { Duration::from_secs(30) }

    async fn tick(&mut self) -> anyhow::Result<()> {
        let now = chrono::Local::now().naive_local();

        for task in &mut self.tasks {
            if task.should_execute(now) {
                tracing::info!(
                    "executing scheduled task {}: {:?} {}",
                    task.id, task.command, task.param
                );
                execute_command(&task.command, &task.param).await?;
                task.last_executed = Some(now);
            }
        }

        Ok(())
    }
}

async fn execute_command(
    cmd: &SchedulerCommand,
    param: &str,
) -> anyhow::Result<()> {
    match cmd {
        SchedulerCommand::PauseDownload => {
            // Send message to download controller to pause
        }
        SchedulerCommand::UnpauseDownload => {
            // Send message to download controller to resume
        }
        SchedulerCommand::DownloadRate => {
            let rate: u32 = param.parse().unwrap_or(0);
            // Update the global download rate limiter
        }
        SchedulerCommand::ActivateServer => {
            let server_id: u32 = param.parse()?;
            // Enable the server in config and reconnect
        }
        SchedulerCommand::DeactivateServer => {
            let server_id: u32 = param.parse()?;
            // Disable the server and drop its connections
        }
        SchedulerCommand::FetchFeed => {
            let feed_id: u32 = param.parse()?;
            // Trigger an immediate RSS feed fetch
        }
        SchedulerCommand::Extensions => {
            // Run the named extension script
        }
        SchedulerCommand::Process => {
            // Run the named system command
        }
        _ => {
            tracing::warn!("unimplemented scheduler command: {cmd:?}");
        }
    }
    Ok(())
}
```

## NZB Directory Scanner

Watches the `NzbDir` directory for incoming `.nzb` files. Files are only processed once their modification time is older than `NzbDirFileAge` seconds, preventing partially-written files from being picked up.

```rust
pub struct NzbDirScanner {
    nzb_dir: PathBuf,
    file_age: Duration,
}

#[async_trait::async_trait]
impl Service for NzbDirScanner {
    fn name(&self) -> &str { "NzbDirScanner" }

    fn interval(&self) -> Duration {
        // Matches config NzbDirInterval (default 5s)
        Duration::from_secs(5)
    }

    async fn tick(&mut self) -> anyhow::Result<()> {
        let mut entries = tokio::fs::read_dir(&self.nzb_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "nzb") {
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

impl NzbDirScanner {
    async fn process_nzb(&self, path: &Path) -> anyhow::Result<()> {
        // 1. Parse the NZB XML file
        // 2. Determine category from filename or subdirectory
        // 3. Check for duplicates (if DupeCheck enabled)
        // 4. Add to download queue
        // 5. Delete or move the NZB file (if NzbCleanupDisk enabled)
        Ok(())
    }
}
```

## Disk Space Monitor

Periodically checks free disk space on `DestDir`, `InterDir`, and `TempDir`. Pauses downloads when any volume drops below the `DiskSpace` threshold and resumes when space is recovered.

```rust
pub struct DiskSpaceMonitor {
    paths: Vec<PathBuf>,
    threshold_mb: u64,
    paused_by_us: bool,
}

#[async_trait::async_trait]
impl Service for DiskSpaceMonitor {
    fn name(&self) -> &str { "DiskSpaceMonitor" }
    fn interval(&self) -> Duration { Duration::from_secs(10) }

    async fn tick(&mut self) -> anyhow::Result<()> {
        for path in &self.paths {
            let available = fs2::available_space(path)? / (1024 * 1024);

            if available < self.threshold_mb {
                if !self.paused_by_us {
                    tracing::warn!(
                        "disk space low on {}: {available} MB < {} MB threshold",
                        path.display(),
                        self.threshold_mb
                    );
                    // Send pause signal to download controller
                    self.paused_by_us = true;
                }
                return Ok(());
            }
        }

        if self.paused_by_us {
            tracing::info!("disk space recovered, resuming downloads");
            // Send resume signal to download controller
            self.paused_by_us = false;
        }

        Ok(())
    }
}
```

## Connection Cleanup

Closes NNTP connections that have been idle beyond a timeout. This frees server-side resources and avoids stale connections that may have been silently dropped by firewalls or the server.

```rust
pub struct ConnectionCleanup {
    pool: Arc<ConnectionPool>,
    idle_timeout: Duration,
}

#[async_trait::async_trait]
impl Service for ConnectionCleanup {
    fn name(&self) -> &str { "ConnectionCleanup" }
    fn interval(&self) -> Duration { Duration::from_secs(30) }

    async fn tick(&mut self) -> anyhow::Result<()> {
        let closed = self.pool.close_idle(self.idle_timeout).await;
        if closed > 0 {
            tracing::debug!("closed {closed} idle NNTP connections");
        }
        Ok(())
    }
}
```

## History Cleanup

Removes completed download entries from history that are older than `KeepHistory` days. Runs once per hour.

```rust
pub struct HistoryCleanup {
    keep_days: u32,
}

#[async_trait::async_trait]
impl Service for HistoryCleanup {
    fn name(&self) -> &str { "HistoryCleanup" }
    fn interval(&self) -> Duration { Duration::from_secs(3600) }

    async fn tick(&mut self) -> anyhow::Result<()> {
        if self.keep_days == 0 {
            return Ok(()); // History retention disabled
        }

        let cutoff = chrono::Utc::now()
            - chrono::Duration::days(self.keep_days as i64);

        // Remove entries older than cutoff from the history store
        // let removed = history.remove_before(cutoff).await?;
        // tracing::info!("removed {removed} history entries older than {cutoff}");

        Ok(())
    }
}
```

## Statistics Tracker

Tracks download volume per server with daily and monthly byte counters. Data is persisted for quota enforcement and the web UI statistics view.

```rust
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct ServerVolume {
    pub bytes_today: u64,
    pub bytes_this_month: u64,
    pub daily_history: Vec<(chrono::NaiveDate, u64)>,
    pub monthly_history: Vec<(chrono::NaiveDate, u64)>,
}

pub struct StatsTracker {
    volumes: HashMap<u32, ServerVolume>,
    last_day: chrono::NaiveDate,
    quota_start_day: u32,
}

#[async_trait::async_trait]
impl Service for StatsTracker {
    fn name(&self) -> &str { "StatsTracker" }
    fn interval(&self) -> Duration { Duration::from_secs(60) }

    async fn tick(&mut self) -> anyhow::Result<()> {
        let today = chrono::Local::now().date_naive();

        if today != self.last_day {
            self.roll_day(today);
        }

        // Persist current counters to disk
        // self.save().await?;

        Ok(())
    }
}

impl StatsTracker {
    fn roll_day(&mut self, today: chrono::NaiveDate) {
        for volume in self.volumes.values_mut() {
            volume.daily_history.push((self.last_day, volume.bytes_today));
            volume.bytes_today = 0;

            // Check for monthly rollover
            if today.day() == self.quota_start_day
                || (self.quota_start_day > 28
                    && today.day() == 1
                    && self.last_day.month() != today.month())
            {
                volume.monthly_history.push((
                    self.last_day,
                    volume.bytes_this_month,
                ));
                volume.bytes_this_month = 0;
            }
        }
        self.last_day = today;
    }

    pub fn record_bytes(&mut self, server_id: u32, bytes: u64) {
        let volume = self.volumes.entry(server_id).or_default();
        volume.bytes_today += bytes;
        volume.bytes_this_month += bytes;
    }

    pub fn check_monthly_quota(
        &self,
        server_id: u32,
        quota_gb: u64,
    ) -> bool {
        if quota_gb == 0 {
            return false; // No quota
        }
        let quota_bytes = quota_gb * 1024 * 1024 * 1024;
        self.volumes
            .get(&server_id)
            .is_some_and(|v| v.bytes_this_month >= quota_bytes)
    }
}
```

## System Health Check

Monitors system-level health: disk write speed, connection liveness, TLS certificate expiry, and queue consistency.

```rust
pub struct HealthChecker {
    dest_dir: PathBuf,
    cert_paths: Vec<PathBuf>,
}

#[async_trait::async_trait]
impl Service for HealthChecker {
    fn name(&self) -> &str { "HealthChecker" }
    fn interval(&self) -> Duration { Duration::from_secs(300) }

    async fn tick(&mut self) -> anyhow::Result<()> {
        self.check_disk_write_speed().await?;
        self.check_certificate_expiry().await?;
        self.check_queue_consistency().await?;
        Ok(())
    }
}

impl HealthChecker {
    async fn check_disk_write_speed(&self) -> anyhow::Result<()> {
        let test_path = self.dest_dir.join(".nzbg_health_check");
        let data = vec![0u8; 1024 * 1024]; // 1 MB
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
        for cert_path in &self.cert_paths {
            // Read and parse the PEM certificate
            // Warn if expiry is within 30 days
        }
        Ok(())
    }

    async fn check_queue_consistency(&self) -> anyhow::Result<()> {
        // Verify queue data files are not corrupt
        // Check that referenced temp files still exist
        // Report any orphaned files
        Ok(())
    }
}
```

## Service Lifecycle

All services follow the same lifecycle, managed by the application's main loop:

```
┌──────────────┐
│  Load Config │
└──────┬───────┘
       │
       ▼
┌──────────────────┐
│  Create Services │
│  - Scheduler     │
│  - NzbDirScanner │
│  - DiskSpaceMon  │
│  - ConnCleanup   │
│  - HistCleanup   │
│  - StatsTracker  │
│  - HealthChecker │
└──────┬───────────┘
       │
       ▼
┌────────────────────────┐
│  Spawn tokio tasks     │
│  (one per service)     │
│  with shutdown channel │
└──────┬─────────────────┘
       │
       ▼
┌────────────────────────┐
│  Services run in loop  │──── tick() at interval()
│  tokio::select! on     │
│  interval + shutdown   │
└──────┬─────────────────┘
       │  (SIGTERM / SIGINT / API shutdown)
       ▼
┌────────────────────────┐
│  Broadcast shutdown    │
│  to all services       │
└──────┬─────────────────┘
       │
       ▼
┌────────────────────────┐
│  Join all tasks        │
│  (graceful completion) │
└────────────────────────┘
```

### Startup Code

```rust
use tokio::sync::broadcast;

pub async fn start_services(
    config: &Config,
) -> anyhow::Result<(broadcast::Sender<()>, Vec<tokio::task::JoinHandle<()>>)> {
    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    let mut handles = Vec::new();

    let services: Vec<Box<dyn Service>> = vec![
        Box::new(Scheduler::from_config(config)),
        Box::new(NzbDirScanner::from_config(config)),
        Box::new(DiskSpaceMonitor::from_config(config)),
        Box::new(ConnectionCleanup::from_config(config)),
        Box::new(HistoryCleanup::from_config(config)),
        Box::new(StatsTracker::from_config(config)),
        Box::new(HealthChecker::from_config(config)),
    ];

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
```
