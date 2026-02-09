# Disk State & Persistence

bergamot persists its runtime state (queue, history, download progress, server statistics, feed state) to disk so that it survives restarts and crashes. This document covers the state directory layout, serialization approach, persistence triggers, partial download resumption, crash-safety strategy, and version migration.

---

## State Directory Layout

All persistent state lives under `QueueDir` (default: `$DataDir/queue`):

```
QueueDir/                          # default: $DataDir/queue
├── queue.json                     # master queue state (NZBs, ordering, IDs)
├── nzb/
│   ├── 00000001.nzb               # original NZB files (retained for redownload)
│   ├── 00000002.nzb
│   └── ...
├── file/
│   ├── 00000001.state             # per-file article completion bitmask
│   ├── 00000002.state
│   └── ...
├── history.json                   # completed/failed NZB history entries
├── stats.json                     # per-server volume statistics (daily/monthly)
├── feeds.json                     # feed history (processed item URLs + status)
└── diskstate.lock                 # advisory lock preventing concurrent access
```

### What Each File Stores

| File | Contents | Written When |
|------|----------|-------------|
| `queue.json` | All active NZBs, their files, ordering, priority, category, pause state | Periodically + on mutations |
| `nzb/*.nzb` | Verbatim copy of the original NZB XML | On NZB add |
| `file/*.state` | Per-file article completion bitmask + CRC values | On article completion (batched) |
| `history.json` | Completed/failed/deleted NZB records with final status | On NZB completion or history mutation |
| `stats.json` | Per-server download volume (bytes/articles per day/month) | Periodically + on shutdown |
| `feeds.json` | Per-feed processed item URLs with fetch/skip status and timestamps | After each feed poll |
| `diskstate.lock` | Empty file used for advisory locking | On startup (held for process lifetime) |

---

## Serialization Format

nzbget uses custom binary/text formats for state persistence. For bergamot, we use **serde** with **JSON** as the primary format for debuggability, with an option to switch to **bincode** for performance if needed.

```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
pub struct QueueState {
    pub version: u32,
    pub nzbs: Vec<NzbState>,
    pub next_nzb_id: u32,
    pub next_file_id: u32,
    pub download_paused: bool,
    pub speed_limit: u64,
}

#[derive(Serialize, Deserialize)]
pub struct NzbState {
    pub id: u32,
    pub name: String,
    pub filename: String,
    pub category: String,
    pub dest_dir: PathBuf,
    pub final_dir: Option<PathBuf>,
    pub priority: i32,
    pub paused: bool,
    pub url: Option<String>,
    pub dupe_key: String,
    pub dupe_score: i32,
    pub dupe_mode: DupeMode,
    pub added_time: chrono::DateTime<chrono::Utc>,
    pub total_size: u64,
    pub downloaded_size: u64,
    pub failed_size: u64,
    pub file_ids: Vec<u32>,
    pub post_process_parameters: HashMap<String, String>,
    pub health: u32,
    pub critical_health: u32,
    pub total_article_count: u32,
    pub success_article_count: u32,
    pub failed_article_count: u32,
}

#[derive(Serialize, Deserialize)]
pub struct HistoryState {
    pub version: u32,
    pub entries: Vec<HistoryEntry>,
}

#[derive(Serialize, Deserialize)]
pub struct ServerStats {
    pub version: u32,
    pub servers: Vec<ServerVolumeStat>,
}

#[derive(Serialize, Deserialize)]
pub struct ServerVolumeStat {
    pub server_id: u32,
    pub server_name: String,
    pub daily_bytes: HashMap<chrono::NaiveDate, u64>,
    pub monthly_bytes: HashMap<String, u64>, // "YYYY-MM" → bytes
    pub total_bytes: u64,
}

#[derive(Serialize, Deserialize)]
pub struct FeedHistoryState {
    pub version: u32,
    pub feeds: HashMap<u32, Vec<FeedHistoryItem>>,
}

#[derive(Serialize, Deserialize)]
pub struct FeedHistoryItem {
    pub url: String,
    pub status: FeedItemStatus,
    pub first_seen: chrono::DateTime<chrono::Utc>,
    pub last_seen: chrono::DateTime<chrono::Utc>,
}
```

### Why JSON over Binary

| Aspect | JSON | bincode |
|--------|------|---------|
| Debuggability | Human-readable, easy to inspect with any text editor | Binary, requires custom tooling |
| Schema evolution | Add fields with `#[serde(default)]`, remove with `#[serde(skip)]` | Must handle versioning manually |
| Size | Larger (~2-5x) | Compact |
| Speed | Slower (~5-10x for large state) | Faster |
| Interop | Readable by external tools (jq, scripts) | Rust-only |

JSON is the default. If profiling shows state serialization as a bottleneck (unlikely — state files are small relative to download data), bincode can be enabled via a feature flag.

### StateFormat Trait

```rust
pub trait StateFormat: Send + Sync {
    fn serialize<T: Serialize>(&self, value: &T) -> Result<Vec<u8>>;
    fn deserialize<T: for<'de> Deserialize<'de>>(&self, data: &[u8]) -> Result<T>;
    fn file_extension(&self) -> &str;
}

pub struct JsonFormat;
impl StateFormat for JsonFormat {
    fn serialize<T: Serialize>(&self, value: &T) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec_pretty(value)?)
    }
    fn deserialize<T: for<'de> Deserialize<'de>>(&self, data: &[u8]) -> Result<T> {
        Ok(serde_json::from_slice(data)?)
    }
    fn file_extension(&self) -> &str {
        "json"
    }
}

#[cfg(feature = "bincode")]
pub struct BincodeFormat;
#[cfg(feature = "bincode")]
impl StateFormat for BincodeFormat {
    fn serialize<T: Serialize>(&self, value: &T) -> Result<Vec<u8>> {
        Ok(bincode::serialize(value)?)
    }
    fn deserialize<T: for<'de> Deserialize<'de>>(&self, data: &[u8]) -> Result<T> {
        Ok(bincode::deserialize(data)?)
    }
    fn file_extension(&self) -> &str {
        "bin"
    }
}
```

---

## DiskState Operations

The `DiskState` struct provides the unified persistence interface. All I/O goes through this struct, which handles atomic writes, directory layout, and format abstraction.

```rust
pub struct DiskState {
    state_dir: PathBuf,
    format: Box<dyn StateFormat>,
}

impl DiskState {
    pub fn new(state_dir: PathBuf, format: Box<dyn StateFormat>) -> Result<Self> {
        std::fs::create_dir_all(&state_dir)?;
        std::fs::create_dir_all(state_dir.join("nzb"))?;
        std::fs::create_dir_all(state_dir.join("file"))?;
        Ok(Self { state_dir, format })
    }

    // ── Queue ──────────────────────────────────────────────
    pub fn save_queue(&self, queue: &QueueState) -> Result<()> {
        let data = self.format.serialize(queue)?;
        let path = self.state_dir.join(format!("queue.{}", self.format.file_extension()));
        atomic_write(&path, &data)
    }

    pub fn load_queue(&self) -> Result<QueueState> {
        let path = self.state_dir.join(format!("queue.{}", self.format.file_extension()));
        let data = std::fs::read(&path)?;
        let state: QueueState = self.format.deserialize(&data)?;
        Ok(state.migrate()?)
    }

    // ── Per-file article tracking ─────────────────────────
    pub fn save_file_state(&self, file_id: u32, state: &FileArticleState) -> Result<()> {
        let data = self.format.serialize(state)?;
        let path = self.state_dir.join(format!("file/{:08}.state", file_id));
        atomic_write(&path, &data)
    }

    pub fn load_file_state(&self, file_id: u32) -> Result<FileArticleState> {
        let path = self.state_dir.join(format!("file/{:08}.state", file_id));
        let data = std::fs::read(&path)?;
        self.format.deserialize(&data)
    }

    pub fn delete_file_state(&self, file_id: u32) -> Result<()> {
        let path = self.state_dir.join(format!("file/{:08}.state", file_id));
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    // ── History ───────────────────────────────────────────
    pub fn save_history(&self, history: &HistoryState) -> Result<()> {
        let data = self.format.serialize(history)?;
        atomic_write(&self.state_dir.join("history.json"), &data)
    }

    pub fn load_history(&self) -> Result<HistoryState> {
        let data = std::fs::read(self.state_dir.join("history.json"))?;
        self.format.deserialize(&data)
    }

    // ── Server stats ──────────────────────────────────────
    pub fn save_stats(&self, stats: &ServerStats) -> Result<()> {
        let data = self.format.serialize(stats)?;
        atomic_write(&self.state_dir.join("stats.json"), &data)
    }

    pub fn load_stats(&self) -> Result<ServerStats> {
        let data = std::fs::read(self.state_dir.join("stats.json"))?;
        self.format.deserialize(&data)
    }

    // ── Feed history ──────────────────────────────────────
    pub fn save_feeds(&self, feeds: &FeedHistoryState) -> Result<()> {
        let data = self.format.serialize(feeds)?;
        atomic_write(&self.state_dir.join("feeds.json"), &data)
    }

    pub fn load_feeds(&self) -> Result<FeedHistoryState> {
        let data = std::fs::read(self.state_dir.join("feeds.json"))?;
        self.format.deserialize(&data)
    }

    // ── NZB file storage ──────────────────────────────────
    pub fn save_nzb_file(&self, id: u32, content: &[u8]) -> Result<()> {
        let path = self.state_dir.join(format!("nzb/{:08}.nzb", id));
        atomic_write(&path, content)
    }

    pub fn load_nzb_file(&self, id: u32) -> Result<Vec<u8>> {
        let path = self.state_dir.join(format!("nzb/{:08}.nzb", id));
        Ok(std::fs::read(&path)?)
    }

    pub fn delete_nzb_file(&self, id: u32) -> Result<()> {
        let path = self.state_dir.join(format!("nzb/{:08}.nzb", id));
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}
```

### Per-File Article State

Each file in the queue has a state file tracking which articles have been successfully downloaded. This enables resumption of partially-downloaded files.

```rust
#[derive(Serialize, Deserialize)]
pub struct FileArticleState {
    pub file_id: u32,
    pub total_articles: u32,
    /// Bitset: bit N = 1 means article N has been downloaded.
    pub completed: Vec<u8>,
    /// CRC32 values for completed articles (for verification).
    pub article_crcs: HashMap<u32, u32>,
}

impl FileArticleState {
    pub fn new(file_id: u32, total_articles: u32) -> Self {
        let byte_count = ((total_articles + 7) / 8) as usize;
        Self {
            file_id,
            total_articles,
            completed: vec![0u8; byte_count],
            article_crcs: HashMap::new(),
        }
    }

    pub fn is_article_done(&self, index: u32) -> bool {
        let byte = (index / 8) as usize;
        let bit = index % 8;
        self.completed.get(byte).map_or(false, |b| b & (1 << bit) != 0)
    }

    pub fn mark_article_done(&mut self, index: u32, crc: u32) {
        let byte = (index / 8) as usize;
        let bit = index % 8;
        if byte >= self.completed.len() {
            self.completed.resize(byte + 1, 0);
        }
        self.completed[byte] |= 1 << bit;
        self.article_crcs.insert(index, crc);
    }

    pub fn completed_count(&self) -> u32 {
        self.completed.iter().map(|b| b.count_ones()).sum()
    }

    pub fn remaining_count(&self) -> u32 {
        self.total_articles - self.completed_count()
    }
}
```

---

## Queue Persistence Triggers

The queue state is saved at these points:

```
┌────────────────────────────────────────────────┐
│              Save Triggers                     │
├────────────────────────────────────────────────┤
│ 1. NZB added to queue                         │
│ 2. NZB removed from queue                     │
│ 3. NZB priority/category/name changed         │
│ 4. Periodic timer (FlushQueue interval)       │
│ 5. Graceful shutdown                          │
│ 6. Article completion (file state only)       │
│ 7. NZB completion (move to history)           │
│ 8. Queue reorder (move up/down/top/bottom)    │
│ 9. NZB pause/resume                           │
└────────────────────────────────────────────────┘
```

The `FlushQueue` config option controls the periodic save interval (default: 250ms during active downloading). This limits disk I/O while bounding data loss on crash to at most one flush interval's worth of progress.

```rust
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

pub struct QueuePersistence {
    disk_state: Arc<DiskState>,
    dirty: AtomicBool,
    flush_interval: Duration,
}

impl QueuePersistence {
    pub fn new(disk_state: Arc<DiskState>, flush_interval: Duration) -> Self {
        Self {
            disk_state,
            dirty: AtomicBool::new(false),
            flush_interval,
        }
    }

    pub fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub async fn flush_loop(
        self: Arc<Self>,
        queue: Arc<tokio::sync::RwLock<QueueState>>,
        mut shutdown: broadcast::Receiver<()>,
    ) {
        let mut interval = tokio::time::interval(self.flush_interval);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if self.dirty.swap(false, Ordering::Relaxed) {
                        if let Err(e) = self.flush(&queue).await {
                            tracing::error!(err = %e, "failed to flush queue state");
                        }
                    }
                }
                _ = shutdown.recv() => {
                    // Final flush on shutdown — always save regardless of dirty flag
                    if let Err(e) = self.flush(&queue).await {
                        tracing::error!(err = %e, "failed final queue flush on shutdown");
                    }
                    break;
                }
            }
        }
    }

    async fn flush(&self, queue: &tokio::sync::RwLock<QueueState>) -> Result<()> {
        let snapshot = queue.read().await.clone();
        self.disk_state.save_queue(&snapshot)
    }
}
```

### FlushQueue Tuning

| `FlushQueue` Value | Trade-off |
|---------------------|-----------|
| `0` | Disable periodic flush; only save on explicit triggers |
| `250` (default) | Save up to 4 times/second during activity; lose ≤250ms of progress on crash |
| `1000` | Lower disk I/O; lose ≤1 second of progress on crash |
| `5000` | Minimal disk I/O; acceptable for HDDs or resource-constrained systems |

---

## Partial Download Resumption

The `ContinuePartial` option controls whether partially downloaded files are resumed after restart:

| Value | Behavior |
|-------|----------|
| `yes` (default) | Resume — load article completion state, skip completed articles |
| `no` | Discard partial file state, re-download everything |

### Resume Flow

```
Startup
  │
  ├── Load queue.json
  │     └── For each NZB, restore file list and ordering
  │
  ├── ContinuePartial == yes?
  │     ├── yes: Load file/*.state for each file in queue
  │     │         └── Mark completed articles as SegmentStatus::Completed
  │     │         └── Scheduler skips completed articles
  │     │
  │     └── no:  Delete all file/*.state files
  │              └── Reset all segments to SegmentStatus::Undefined
  │              └── Delete partial download files from temp directory
  │
  └── Begin downloading from first uncompleted article
```

### Article Completion Tracking During Download

```rust
impl QueueCoordinator {
    async fn handle_article_success(
        &mut self,
        file_id: u32,
        article_index: u32,
        crc: u32,
    ) {
        // Update in-memory state
        self.queue.set_segment_status(file_id, article_index, SegmentStatus::Completed);

        // Update on-disk article state (batched via flush timer)
        if let Some(file_state) = self.file_states.get_mut(&file_id) {
            file_state.mark_article_done(article_index, crc);
        }

        // Mark file state as dirty for next flush
        self.persistence.mark_dirty();
    }
}
```

---

## Crash Recovery

### Atomic Writes

All state files are written atomically using the write-to-temp-then-rename pattern:

```rust
use std::fs;
use std::io::Write;
use std::path::Path;

pub fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp_path = path.with_extension("tmp");

    let mut file = fs::File::create(&tmp_path)?;
    file.write_all(data)?;
    file.sync_all()?; // fsync for durability

    fs::rename(&tmp_path, path)?;

    // fsync the parent directory to ensure the rename is durable
    if let Some(parent) = path.parent() {
        let dir = fs::File::open(parent)?;
        dir.sync_all()?;
    }

    Ok(())
}
```

This guarantees that a state file is either the old complete version or the new complete version — never a partial write. The sequence is:

1. Write data to `path.tmp`
2. `fsync` the temp file (data is durable on disk)
3. `rename` temp → target (atomic on POSIX)
4. `fsync` the parent directory (rename is durable)

### Advisory Lock

On startup, bergamot acquires an advisory lock on `diskstate.lock` to prevent two instances from corrupting shared state:

```rust
use fs2::FileExt;
use std::fs::File;
use std::path::Path;

pub struct StateLock {
    _lock_file: File,
}

impl StateLock {
    pub fn acquire(state_dir: &Path) -> Result<Self> {
        let lock_path = state_dir.join("diskstate.lock");
        let lock_file = File::create(&lock_path)?;
        lock_file.try_lock_exclusive().map_err(|_| {
            anyhow::anyhow!(
                "another bergamot instance is already running (lock held on {})",
                lock_path.display()
            )
        })?;
        Ok(Self { _lock_file: lock_file })
    }
}

// Lock is released automatically when StateLock is dropped
```

### Recovery Procedure

On startup, `DiskState` performs a recovery sequence:

```
┌─────────────────────────────────────────────────┐
│                Startup Recovery                  │
├─────────────────────────────────────────────────┤
│ 1. Acquire advisory lock on diskstate.lock      │
│ 2. Clean up .tmp files (incomplete writes)      │
│ 3. Load queue.json                              │
│    ├── success → validate + migrate             │
│    └── missing/corrupt → log error, empty queue │
│ 4. Load file/*.state for each file in queue     │
│    ├── missing → reset file to re-download      │
│    └── present → restore completion bitmask     │
│ 5. Load history.json                            │
│    └── missing/corrupt → empty history          │
│ 6. Load stats.json                              │
│    └── missing/corrupt → empty stats            │
│ 7. Load feeds.json                              │
│    └── missing/corrupt → empty feed history     │
│ 8. Validate consistency:                        │
│    ├── file IDs in queue → must have state files│
│    ├── NZB IDs in queue → must have nzb/ files  │
│    └── orphaned state files → log + delete      │
└─────────────────────────────────────────────────┘
```

```rust
impl DiskState {
    pub fn recover(&self) -> Result<RecoveryReport> {
        let mut report = RecoveryReport::default();

        // Clean up incomplete writes in all directories
        for dir in &[&self.state_dir, &self.state_dir.join("file"), &self.state_dir.join("nzb")] {
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.flatten() {
                    if entry.path().extension() == Some("tmp".as_ref()) {
                        tracing::warn!(path = %entry.path().display(), "removing incomplete state file");
                        fs::remove_file(entry.path())?;
                        report.tmp_files_cleaned += 1;
                    }
                }
            }
        }

        Ok(report)
    }

    pub fn validate_consistency(
        &self,
        queue: &QueueState,
    ) -> Result<Vec<ConsistencyWarning>> {
        let mut warnings = Vec::new();

        for nzb in &queue.nzbs {
            // Check NZB file exists
            let nzb_path = self.state_dir.join(format!("nzb/{:08}.nzb", nzb.id));
            if !nzb_path.exists() {
                warnings.push(ConsistencyWarning::MissingNzbFile { nzb_id: nzb.id });
            }

            // Check file state files exist
            for &file_id in &nzb.file_ids {
                let state_path = self.state_dir.join(format!("file/{:08}.state", file_id));
                if !state_path.exists() {
                    warnings.push(ConsistencyWarning::MissingFileState { file_id });
                }
            }
        }

        // Check for orphaned state files
        if let Ok(entries) = fs::read_dir(self.state_dir.join("file")) {
            let known_ids: std::collections::HashSet<u32> = queue.nzbs.iter()
                .flat_map(|n| &n.file_ids)
                .copied()
                .collect();

            for entry in entries.flatten() {
                if let Some(id) = Self::parse_file_id(&entry.path()) {
                    if !known_ids.contains(&id) {
                        warnings.push(ConsistencyWarning::OrphanedFileState { file_id: id });
                    }
                }
            }
        }

        Ok(warnings)
    }
}

#[derive(Default)]
pub struct RecoveryReport {
    pub tmp_files_cleaned: u32,
}

pub enum ConsistencyWarning {
    MissingNzbFile { nzb_id: u32 },
    MissingFileState { file_id: u32 },
    OrphanedFileState { file_id: u32 },
}
```

---

## Version Migration

State files include a `version` field. When loading state from a different version, the migration system applies incremental transformations:

```rust
impl QueueState {
    pub fn migrate(mut self) -> Result<Self> {
        loop {
            match self.version {
                1 => {
                    // v1 → v2: added dupe_mode field
                    for nzb in &mut self.nzbs {
                        if nzb.dupe_mode == DupeMode::default() {
                            nzb.dupe_mode = DupeMode::Score;
                        }
                    }
                    self.version = 2;
                }
                2 => {
                    // v2 → v3: added health tracking fields
                    for nzb in &mut self.nzbs {
                        if nzb.health == 0 && nzb.total_article_count > 0 {
                            nzb.health = 1000; // assume healthy
                        }
                    }
                    self.version = 3;
                }
                3 => return Ok(self), // current version
                v => return Err(anyhow::anyhow!("unsupported state version: {v}")),
            }
        }
    }
}
```

### Serde Defaults for Forward Compatibility

Use `#[serde(default)]` on new fields so that old state files deserialize cleanly without explicit migration for simple additions:

```rust
#[derive(Serialize, Deserialize)]
pub struct NzbState {
    pub id: u32,
    pub name: String,
    // ... existing fields ...

    // New field added in v3 — old state files will get the default value
    #[serde(default)]
    pub health: u32,

    // New optional field — old state files will get None
    #[serde(default)]
    pub added_by: Option<String>,
}
```

### Migration Strategy

| Change Type | Approach |
|-------------|----------|
| Add optional field | `#[serde(default)]` — no migration code needed |
| Add required field with sensible default | `#[serde(default)]` + compute correct value in migration |
| Rename field | `#[serde(alias = "old_name")]` for reading + migration to write new name |
| Remove field | `#[serde(skip)]` or simply remove (unknown fields ignored with `#[serde(deny_unknown_fields)]` off) |
| Change field type | Explicit migration step with version bump |

---

## Integration with QueueCoordinator

The `QueueCoordinator` (see [05-queue-coordinator.md](05-queue-coordinator.md)) owns the `DiskState` and coordinates persistence:

```
┌──────────────────────────────────────────────────────┐
│                  QueueCoordinator                     │
│                                                       │
│  ┌──────────────┐    ┌────────────────────┐           │
│  │ DownloadQueue│    │ QueuePersistence   │           │
│  │  (in-memory) │───►│  (flush timer)     │           │
│  └──────────────┘    └────────┬───────────┘           │
│                               │                       │
│                               ▼                       │
│                      ┌────────────────┐               │
│                      │   DiskState    │               │
│                      │ (atomic I/O)   │               │
│                      └────────────────┘               │
└──────────────────────────────────────────────────────┘
```

The coordinator calls `persistence.mark_dirty()` on every mutation. The persistence layer's flush loop checks the dirty flag and writes state to disk at the configured interval. On shutdown, a final flush is always performed regardless of the dirty flag.
