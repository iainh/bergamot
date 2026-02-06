# Logging & History

nzbg provides a multi-target logging system and a history system that records the final state of every completed or failed download.

## Log Levels

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Debug = 0,
    Detail = 1,
    Info = 2,
    Warning = 3,
    Error = 4,
}
```

Each level corresponds to a `tracing` severity and a nzbget `MessageKind`:

| LogLevel | tracing | MessageKind (API value) |
|----------|---------|------------------------|
| `Debug` | `tracing::debug!` | `"debug"` |
| `Detail` | `tracing::info!` (with field) | `"detail"` |
| `Info` | `tracing::info!` | `"info"` |
| `Warning` | `tracing::warn!` | `"warning"` |
| `Error` | `tracing::error!` | `"error"` |

## Log Targets

Each log level can be directed to different targets:

```
┌─────────┐     ┌──────────┐     ┌──────────────┐
│ Message │────►│ LogRouter│────►│ Console      │ (stdout/stderr)
│         │     │          │────►│ Log File     │ (append mode)
│         │     │          │────►│ Ring Buffer  │ (for web UI)
│         │     │          │────►│ Per-NZB Log  │ (optional)
└─────────┘     └──────────┘     └──────────────┘
```

Configuration options:

| Option | Values | Description |
|--------|--------|-------------|
| `InfoTarget` | `screen`, `log`, `both`, `none` | Where to send Info messages |
| `WarningTarget` | `screen`, `log`, `both`, `none` | Where to send Warning messages |
| `ErrorTarget` | `screen`, `log`, `both`, `none` | Where to send Error messages |
| `DebugTarget` | `screen`, `log`, `both`, `none` | Where to send Debug messages |
| `DetailTarget` | `screen`, `log`, `both`, `none` | Where to send Detail messages |

## MessageKind and Log Struct

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogMessage {
    pub id: u32,
    pub kind: LogLevel,
    pub time: chrono::DateTime<chrono::Utc>,
    pub text: String,
    pub nzb_id: Option<u32>,
}
```

## Structured Logging with `tracing`

nzbg uses the `tracing` crate for structured, async-aware logging. A custom `tracing::Subscriber` layer routes messages to the appropriate targets.

```rust
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

pub fn init_logging(config: &LogConfig) -> Result<LogHandle> {
    let log_buffer = LogBuffer::new(config.log_buffer_size);
    let file_writer = config.log_file.as_ref().map(|path| {
        FileLogWriter::new(path, config.rotate_log)
    }).transpose()?;

    let nzbg_layer = NzbgLogLayer {
        buffer: log_buffer.clone(),
        file_writer,
        targets: config.targets.clone(),
    };

    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info")))
        .with(nzbg_layer)
        .init();

    Ok(LogHandle { buffer: log_buffer })
}
```

### Custom Layer

```rust
use tracing::Subscriber;
use tracing_subscriber::Layer;

pub struct NzbgLogLayer {
    pub buffer: LogBuffer,
    pub file_writer: Option<FileLogWriter>,
    pub targets: LogTargets,
}

impl<S: Subscriber> Layer<S> for NzbgLogLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let level = Self::to_log_level(event.metadata().level());
        let target = self.targets.get(level);

        let message = Self::format_event(event);

        // Route to configured targets
        if target.screen() {
            eprintln!("{}", message);
        }
        if target.log_file() {
            if let Some(ref writer) = self.file_writer {
                writer.write(&message);
            }
        }

        // Always add to ring buffer (for web UI)
        self.buffer.push(LogMessage {
            id: self.buffer.next_id(),
            kind: level,
            time: chrono::Utc::now(),
            text: message,
            nzb_id: Self::extract_nzb_id(event),
        });
    }
}
```

## Log File Management

### Modes

| `WriteLog` | Behavior |
|------------|----------|
| `append` | Append to existing log file |
| `rotate` | Rotate log files daily |
| `none` | No log file output |

### Rotation

When `WriteLog = rotate`, log files are named with date suffixes and old files are deleted based on `RotateLog` (number of days to keep):

```rust
pub struct FileLogWriter {
    path: PathBuf,
    mode: WriteLogMode,
    rotate_days: u32,
    current_file: Mutex<Option<BufWriter<File>>>,
    current_date: Mutex<chrono::NaiveDate>,
}

impl FileLogWriter {
    pub fn write(&self, message: &str) {
        let today = chrono::Utc::now().date_naive();
        let mut current_date = self.current_date.lock().unwrap();

        if self.mode == WriteLogMode::Rotate && today != *current_date {
            self.rotate(today);
            *current_date = today;
        }

        let mut file = self.current_file.lock().unwrap();
        if let Some(ref mut f) = *file {
            let _ = writeln!(f, "{}", message);
        }
    }

    fn rotate(&self, today: chrono::NaiveDate) {
        // Close current file
        let mut file = self.current_file.lock().unwrap();
        *file = None;

        // Rename current log to date-suffixed name
        let dated_name = self.path.with_extension(
            format!("{}.log", today.format("%Y-%m-%d"))
        );
        let _ = fs::rename(&self.path, &dated_name);

        // Open new file
        *file = Some(BufWriter::new(
            File::create(&self.path).expect("failed to create log file"),
        ));

        // Purge old rotated logs
        self.purge_old_logs(today);
    }

    fn purge_old_logs(&self, today: chrono::NaiveDate) {
        let cutoff = today - chrono::Duration::days(self.rotate_days as i64);
        // Delete log files older than cutoff based on filename date
        if let Some(parent) = self.path.parent() {
            for entry in fs::read_dir(parent).into_iter().flatten() {
                if let Ok(entry) = entry {
                    if let Some(date) = Self::parse_log_date(&entry.path()) {
                        if date < cutoff {
                            let _ = fs::remove_file(entry.path());
                        }
                    }
                }
            }
        }
    }
}
```

## Log Buffer (Ring Buffer)

An in-memory ring buffer holds recent log messages for the web UI's log display and the `log` API method.

```rust
use std::sync::Mutex;

pub struct LogBuffer {
    messages: Mutex<VecDeque<LogMessage>>,
    capacity: usize,
    next_id: AtomicU32,
}

impl LogBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            messages: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
            next_id: AtomicU32::new(1),
        }
    }

    pub fn push(&self, mut msg: LogMessage) {
        msg.id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut msgs = self.messages.lock().unwrap();
        if msgs.len() >= self.capacity {
            msgs.pop_front();
        }
        msgs.push_back(msg);
    }

    /// Retrieve messages with ID > since_id. Used by the API.
    pub fn messages_since(&self, since_id: u32) -> Vec<LogMessage> {
        let msgs = self.messages.lock().unwrap();
        msgs.iter()
            .filter(|m| m.id > since_id)
            .cloned()
            .collect()
    }
}
```

The `LogBuffer` config option controls capacity (default: 1000 messages).

## Per-NZB Logging

When `NzbLog = yes`, log messages associated with a specific NZB (via the `nzb_id` field in structured logging) are stored alongside that NZB's state. This allows the UI to show a per-download log.

```rust
impl NzbState {
    pub fn add_log_message(&mut self, msg: LogMessage) {
        self.log_messages.push(msg);
    }
}
```

Messages are associated using `tracing` span fields:

```rust
let _span = tracing::info_span!("nzb", nzb_id = nzb.id).entered();
tracing::info!("starting post-processing");
// The NzbgLogLayer extracts nzb_id from the span context
```

## History System

### HistoryCoordinator

The `HistoryCoordinator` manages the lifecycle of completed downloads:

```
┌───────────┐     completion      ┌──────────────────┐
│  Queue    │ ──────────────────► │ HistoryCoordinator│
│           │                     │                   │
│           │  ◄── return to q ── │  history entries  │
│           │                     │  retention mgmt   │
└───────────┘                     └──────────────────┘
```

```rust
pub struct HistoryCoordinator {
    entries: Vec<HistoryEntry>,
    keep_history_days: u32,
    disk_state: Arc<DiskState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: u32,
    pub nzb_name: String,
    pub nzb_filename: String,
    pub category: String,
    pub dest_dir: PathBuf,
    pub final_dir: Option<PathBuf>,
    pub url: Option<String>,

    // Status
    pub status: HistoryStatus,
    pub total_status: TotalStatus,
    pub par_status: ParStatus,
    pub unpack_status: UnpackStatus,
    pub script_statuses: Vec<ScriptStatus>,

    // Timing
    pub added_time: chrono::DateTime<chrono::Utc>,
    pub completed_time: chrono::DateTime<chrono::Utc>,
    pub download_time_sec: u64,
    pub post_process_time_sec: u64,

    // Statistics
    pub size: u64,
    pub downloaded_size: u64,
    pub file_count: u32,
    pub article_count: u32,
    pub failed_articles: u32,
    pub health: f32,

    // Duplicate info
    pub dupe_key: String,
    pub dupe_score: i32,
    pub dupe_mode: DupeMode,

    // Retention
    pub delete_status: DeleteStatus,
    pub mark: HistoryMark,
}
```

### History Status Enums

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TotalStatus {
    Success,
    Warning,
    Failure,
    Deleted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ParStatus {
    None,
    Failure,
    Success,
    RepairPossible,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum UnpackStatus {
    None,
    Failure,
    Success,
    PasswordRequired,
    Space,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HistoryMark {
    None,
    Bad,
    Good,
    Success,
}
```

### History Operations

| Operation | API Method | Description |
|-----------|-----------|-------------|
| Delete | `editqueue("HistoryDelete", ...)` | Remove entry permanently |
| Return | `editqueue("HistoryReturn", ...)` | Return NZB to download queue |
| Redownload | `editqueue("HistoryRedownload", ...)` | Re-add and re-download |
| Mark Bad | `editqueue("HistoryMarkBad", ...)` | Mark as bad (for dupe system) |
| Mark Good | `editqueue("HistoryMarkGood", ...)` | Mark as good |
| Mark Success | `editqueue("HistoryMarkSuccess", ...)` | Override status to success |

```rust
impl HistoryCoordinator {
    pub fn add_completed(&mut self, nzb: NzbState, result: PostProcessResult) {
        let entry = HistoryEntry::from_completed(nzb, result);
        self.entries.push(entry);
        self.purge_expired();
    }

    pub fn return_to_queue(&mut self, id: u32) -> Option<NzbState> {
        let idx = self.entries.iter().position(|e| e.id == id)?;
        let entry = self.entries.remove(idx);
        Some(entry.to_nzb_state())
    }

    pub fn mark(&mut self, id: u32, mark: HistoryMark) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            entry.mark = mark;
        }
    }

    fn purge_expired(&mut self) {
        if self.keep_history_days == 0 {
            return; // keep forever
        }
        let cutoff = chrono::Utc::now()
            - chrono::Duration::days(self.keep_history_days as i64);
        self.entries.retain(|e| e.completed_time > cutoff);
    }
}
```

### History Retention

The `KeepHistory` option controls how long history entries are kept:

| Value | Behavior |
|-------|----------|
| `0` | Keep forever |
| `N` | Keep for N days, then auto-purge |

Purging runs after every new entry is added and periodically (e.g., daily).
