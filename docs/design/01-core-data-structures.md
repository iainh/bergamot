# 01 — Core Data Structures

This document defines every major data structure in bergamot, their
relationships, ownership semantics, and Rust representations. These
structures mirror NZBGet's internal model while adapting to Rust
idioms.

---

## Object Hierarchy

```
DownloadQueue
├── queue: Vec<NzbInfo>                    ← active / queued downloads
│   └── NzbInfo
│       ├── files: Vec<FileInfo>           ← files within the NZB
│       │   └── FileInfo
│       │       └── articles: Vec<ArticleInfo>   ← yEnc segments
│       ├── completed_files: Vec<CompletedFile>
│       ├── server_stats: Vec<ServerStat>
│       └── post_info: Option<PostInfo>    ← present during post-processing
├── history: Vec<HistoryInfo>              ← completed / failed / hidden
└── next_nzb_id / next_file_id: counters
```

---

## NzbInfo

`NzbInfo` is the central structure representing a single NZB download
job. It tracks everything from the moment an NZB is added through
downloading, post-processing, and final placement.

```rust
#[derive(Debug, Clone)]
pub struct NzbInfo {
    // ── Identity ────────────────────────────────────────────────
    pub id: u32,
    pub kind: NzbKind,
    pub name: String,
    pub filename: String,
    pub url: String,

    // ── Directories ─────────────────────────────────────────────
    pub dest_dir: PathBuf,
    pub final_dir: PathBuf,
    pub temp_dir: PathBuf,
    pub queue_dir: PathBuf,

    // ── Classification ──────────────────────────────────────────
    pub category: String,
    pub priority: Priority,
    pub dup_key: String,
    pub dup_mode: DupMode,
    pub dup_score: i32,

    // ── Size tracking (bytes) ───────────────────────────────────
    pub size: u64,
    pub remaining_size: u64,
    pub paused_size: u64,
    pub failed_size: u64,
    pub success_size: u64,
    pub current_downloaded_size: u64,
    pub par_size: u64,
    pub par_remaining_size: u64,
    pub par_current_success_size: u64,
    pub par_failed_size: u64,

    // ── Article / file counts ───────────────────────────────────
    pub file_count: u32,
    pub remaining_file_count: u32,
    pub remaining_par_count: u32,
    pub total_article_count: u32,
    pub success_article_count: u32,
    pub failed_article_count: u32,

    // ── Timing ──────────────────────────────────────────────────
    pub added_time: SystemTime,
    pub min_time: Option<SystemTime>,
    pub max_time: Option<SystemTime>,
    pub download_start_time: Option<SystemTime>,
    pub download_sec: u64,
    pub post_total_sec: u64,
    pub par_sec: u64,
    pub repair_sec: u64,
    pub unpack_sec: u64,

    // ── Status flags ────────────────────────────────────────────
    pub paused: bool,
    pub deleted: bool,
    pub direct_rename: bool,
    pub force_priority: bool,
    pub reprocess: bool,
    pub par_manual: bool,
    pub clean_up_disk: bool,

    // ── Status enumerations ─────────────────────────────────────
    pub par_status: ParStatus,
    pub unpack_status: UnpackStatus,
    pub move_status: MoveStatus,
    pub delete_status: DeleteStatus,
    pub mark_status: MarkStatus,
    pub url_status: UrlStatus,

    // ── Health ──────────────────────────────────────────────────
    /// Current health: ratio of expected vs available articles (0–1000).
    pub health: u32,
    /// Critical health: below this threshold repair is impossible.
    pub critical_health: u32,

    // ── Sub-structures ──────────────────────────────────────────
    pub files: Vec<FileInfo>,
    pub completed_files: Vec<CompletedFile>,
    pub server_stats: Vec<ServerStat>,
    pub parameters: Vec<NzbParameter>,
    pub post_info: Option<PostInfo>,

    // ── Messages ────────────────────────────────────────────────
    pub message_count: u32,
    pub cached_message_count: u32,
}
```

### NzbKind

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NzbKind {
    /// Standard NZB file containing article references.
    Nzb,
    /// URL-based download — the NZB itself must first be fetched.
    Url,
}
```

### Priority

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    VeryLow  = -100,
    Low      = -50,
    Normal   = 0,
    High     = 50,
    VeryHigh = 100,
    Force    = 900,
}
```

---

## Status Enumerations

Every status enum uses explicit discriminants so that values are stable
across serialization and the RPC API.

### ParStatus

Tracks PAR2 verification and repair outcome.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParStatus {
    /// Not yet checked / not applicable.
    None         = 0,
    /// PAR2 check failed (and repair was not possible).
    Failure      = 1,
    /// PAR2 check passed — all files intact.
    Success      = 2,
    /// PAR2 repair succeeded — damaged files were reconstructed.
    RepairPossible = 3,
    /// Manual PAR2 verification requested by user.
    Manual       = 4,
}
```

### UnpackStatus

Tracks archive extraction outcome.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnpackStatus {
    /// Not yet attempted / not applicable.
    None      = 0,
    /// Extraction failed.
    Failure   = 1,
    /// Extraction succeeded.
    Success   = 2,
    /// Archive appears to require a password not supplied.
    Password  = 3,
    /// Disk space insufficient for extraction.
    Space     = 4,
}
```

### MoveStatus

Tracks the final-directory move step.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveStatus {
    /// Not yet moved.
    None    = 0,
    /// Move failed.
    Failure = 1,
    /// Move succeeded.
    Success = 2,
}
```

### DeleteStatus

Records why a download was deleted.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteStatus {
    /// Not deleted.
    None    = 0,
    /// Manually deleted by user.
    Manual  = 1,
    /// Deleted because health dropped below critical threshold.
    Health  = 2,
    /// Deleted because it was identified as a duplicate.
    Dupe    = 3,
    /// Deleted because a bad or missing password was detected.
    Bad     = 4,
    /// Deleted because a scan script rejected the NZB.
    Scan    = 5,
    /// Deleted by an extension script.
    Copy    = 6,
}
```

### MarkStatus

Records whether a download was marked good/bad by the user.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkStatus {
    None    = 0,
    Good    = 1,
    Bad     = 2,
    Success = 3,
}
```

### UrlStatus

Status for URL-type NZB entries (fetching the NZB file itself).

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlStatus {
    /// Not yet processed.
    None          = 0,
    /// URL fetch failed.
    Failure       = 1,
    /// URL fetch succeeded — NZB has been added to the queue.
    Success       = 2,
    /// Fetch failed — could not reach server.
    ScanFailure   = 3,
    /// Skipped — a scan script rejected this URL.
    ScanSkipped   = 4,
}
```

### DupMode

Controls how duplicate detection behaves for this NZB.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DupMode {
    /// Use the global duplicate-check setting.
    Score = 0,
    /// Keep all duplicates (no dedup).
    All   = 1,
    /// Keep only the best-scored duplicate.
    Force = 2,
}
```

---

## FileInfo

Represents a single file within an NZB — typically one output file
assembled from multiple NNTP articles.

```rust
#[derive(Debug, Clone)]
pub struct FileInfo {
    // ── Identity ────────────────────────────────────────────────
    pub id: u32,
    pub nzb_id: u32,

    // ── Names ───────────────────────────────────────────────────
    /// Original filename extracted from the article subject.
    pub filename: String,
    /// Full Usenet subject line.
    pub subject: String,
    /// Filename to write on disk (may differ after rename detection).
    pub output_filename: String,

    // ── Usenet metadata ─────────────────────────────────────────
    pub groups: Vec<String>,
    pub articles: Vec<ArticleInfo>,

    // ── Size tracking ───────────────────────────────────────────
    pub size: u64,
    pub remaining_size: u64,
    pub success_size: u64,
    pub failed_size: u64,
    pub missed_size: u64,

    // ── Article counts ──────────────────────────────────────────
    pub total_articles: u32,
    pub missing_articles: u32,
    pub failed_articles: u32,
    pub success_articles: u32,

    // ── State ───────────────────────────────────────────────────
    pub paused: bool,
    pub completed: bool,
    pub priority: Priority,
    pub time: SystemTime,
    pub active_downloads: u32,

    // ── Integrity ───────────────────────────────────────────────
    /// Expected CRC-32 of the fully decoded output file (from yEnc trailer).
    pub crc: u32,
    pub server_stats: Vec<ServerStat>,
}
```

### File Ordering Within an NZB

The order of `FileInfo` entries in `NzbInfo::files` is significant and
follows NZBGet's strategy for efficient PAR2 handling:

| Position | File type            | Paused? | Rationale                                                   |
|----------|----------------------|---------|-------------------------------------------------------------|
| 1        | PAR2 main (`.par2`)  | No      | Needed early to determine block counts and verify files.    |
| 2 … N-M  | Data files           | No      | The actual content files — downloaded in parallel.          |
| N-M+1 … N| PAR2 repair volumes  | **Yes** | Only unpaused if verification detects missing/damaged blocks.|

This ordering means repair blocks consume no bandwidth unless needed.
The queue coordinator unpauses the minimum number of repair volumes
required, based on the block deficit reported by PAR2 verification.

```
files[0]     →  show.par2                  (main, active)
files[1]     →  show.mkv                   (data, active)
files[2]     →  show.nfo                   (data, active)
files[3]     →  show.vol00+01.par2         (repair, paused)
files[4]     →  show.vol01+02.par2         (repair, paused)
files[5]     →  show.vol03+04.par2         (repair, paused)
```

---

## ArticleInfo

Represents a single NNTP article (one yEnc segment of a file).

```rust
#[derive(Debug, Clone)]
pub struct ArticleInfo {
    /// 1-based part number within the parent file.
    pub part_number: u32,

    /// Unique Usenet message-id (e.g., `<abc123@news.example.com>`).
    pub message_id: String,

    /// Expected encoded article size in bytes (from NZB `<segment>`).
    pub size: u64,

    /// Current download status.
    pub status: ArticleStatus,

    /// Byte offset within the decoded output file where this segment starts.
    pub segment_offset: u64,

    /// Decoded segment size in bytes.
    pub segment_size: u64,

    /// CRC-32 of the decoded segment (from yEnc `=yend` trailer).
    pub crc: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArticleStatus {
    /// Not yet attempted.
    Undefined = 0,
    /// Currently being downloaded.
    Running   = 1,
    /// Successfully downloaded and decoded.
    Finished  = 2,
    /// All servers exhausted — article is missing.
    Failed    = 3,
}
```

**Segment offset & size** are populated during yEnc decoding. They
allow the output file to be written out of order (via `pwrite` /
`seek+write`) as articles complete on different connections
concurrently.

---

## DownloadQueue

The top-level container, owned by the queue coordinator.

```rust
pub struct DownloadQueue {
    /// Active and queued NZB downloads, ordered by effective priority.
    pub queue: Vec<NzbInfo>,

    /// Completed, failed, and hidden history entries.
    pub history: Vec<HistoryInfo>,

    /// Monotonically increasing ID generator for NZBs.
    pub next_nzb_id: u32,

    /// Monotonically increasing ID generator for files.
    pub next_file_id: u32,
}
```

In NZBGet, this structure is guarded by a global mutex
(`DownloadQueue::Lock()`). In bergamot, we use a different concurrency
model — see **Thread Safety** below.

---

## HistoryInfo

After a download completes (or fails, or is deleted), its `NzbInfo` is
moved into history wrapped in a `HistoryInfo`.

```rust
#[derive(Debug, Clone)]
pub struct HistoryInfo {
    pub id: u32,
    pub kind: HistoryKind,
    pub time: SystemTime,
    pub nzb_info: NzbInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryKind {
    /// Normal completed / failed NZB.
    Nzb       = 0,
    /// URL-type entry.
    Url       = 1,
    /// Hidden duplicate — kept for dup-score tracking but not shown by default.
    DupHidden = 2,
}
```

---

## ServerStat

Per-server download statistics, tracked at both the `NzbInfo` and
`FileInfo` level.

```rust
#[derive(Debug, Clone)]
pub struct ServerStat {
    /// Server ID (matches configuration).
    pub server_id: u32,
    /// Articles successfully downloaded from this server.
    pub success_articles: u32,
    /// Articles that failed on this server.
    pub failed_articles: u32,
}
```

---

## CompletedFile

Records a file that has been fully downloaded and written to disk.

```rust
#[derive(Debug, Clone)]
pub struct CompletedFile {
    /// Filename on disk (relative to the NZB temp/dest directory).
    pub filename: String,

    /// Original filename from the article subject (before rename).
    pub original_filename: String,

    /// Status of this completed file.
    pub status: CompletedFileStatus,

    /// CRC-32 of the decoded file.
    pub crc: u32,

    /// Unique ID (mirrors the `FileInfo::id` that produced this file).
    pub id: u32,

    /// File size in bytes.
    pub size: u64,

    /// PAR2 hash for verification (if available).
    pub hash: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletedFileStatus {
    /// Download succeeded — file is intact.
    Success       = 0,
    /// Download failed — file is incomplete or missing.
    Failure       = 1,
    /// File was part of a dupe group and hidden.
    Dupe          = 2,
    /// File was explicitly deleted during post-processing.
    Deleted       = 3,
}
```

---

## NzbParameter

User-defined or script-defined key-value pairs attached to a download.
Parameters are passed to extension scripts as environment variables
(prefixed with `NZBOP_` / `NZBPR_`).

```rust
#[derive(Debug, Clone)]
pub struct NzbParameter {
    pub name: String,
    pub value: String,
}
```

---

## PostInfo

Tracks the state of post-processing for a single NZB. Only present
while post-processing is active.

```rust
#[derive(Debug, Clone)]
pub struct PostInfo {
    pub nzb_id: u32,
    pub stage: PostStage,
    pub progress_label: String,
    pub file_progress: f32,
    pub stage_progress: f32,
    pub start_time: SystemTime,
    pub stage_time: SystemTime,
    pub working: bool,
    pub messages: Vec<PostMessage>,
}
```

### PostStage

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostStage {
    /// Queued for post-processing, not yet started.
    Queued          = 0,
    /// Loading PAR2 file list for verification.
    ParLoading      = 1,
    /// Renaming obfuscated files using PAR2 data.
    ParRenaming     = 2,
    /// Verifying file integrity with PAR2.
    ParVerifying    = 3,
    /// Repairing damaged files with PAR2 repair volumes.
    ParRepairing    = 4,
    /// Extracting archives (rar, 7z, zip).
    Unpacking       = 5,
    /// Moving files from temp directory to final directory.
    Moving          = 6,
    /// Running user-defined post-processing scripts.
    Executing       = 7,
    /// Post-processing complete.
    Finished        = 8,
}
```

### PostMessage

```rust
#[derive(Debug, Clone)]
pub struct PostMessage {
    pub kind: PostMessageKind,
    pub text: String,
    pub time: SystemTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostMessageKind {
    Info    = 0,
    Warning = 1,
    Error   = 2,
    Detail  = 3,
}
```

---

## Thread Safety & Concurrency Model

NZBGet protects `DownloadQueue` with a single global mutex that every
thread (downloader, post-processor, RPC handler, scheduler) must
acquire. This is simple but creates contention under high concurrency.

In Rust we have three practical options:

### Option A: `Arc<RwLock<DownloadQueue>>`

Mirrors NZBGet most closely. Multiple readers can inspect the queue
concurrently; writers acquire exclusive access. Simple to implement,
but still risks lock contention and potential deadlocks when nested
locking is needed.

### Option B: Actor Model (Recommended)

The queue coordinator runs as a single tokio task that owns
`DownloadQueue` exclusively. All other components interact through
typed message channels:

```
┌───────────────┐     QueueCommand      ┌─────────────────────┐
│  RPC handler  │ ──────────────────▶   │                     │
├───────────────┤                       │  Queue Coordinator  │
│  Downloader   │ ──────────────────▶   │  (owns queue)       │
├───────────────┤                       │                     │
│  Scheduler    │ ──────────────────▶   │  Responds via       │
├───────────────┤                       │  oneshot channels    │
│  PostProc     │ ──────────────────▶   │                     │
└───────────────┘                       └─────────────────────┘
```

```rust
pub enum QueueCommand {
    AddNzb { nzb: NzbInfo, reply: oneshot::Sender<u32> },
    Pause { id: u32, reply: oneshot::Sender<bool> },
    Remove { id: u32, reply: oneshot::Sender<bool> },
    GetStatus { reply: oneshot::Sender<QueueSnapshot> },
    GetFileForDownload { server_id: u32, reply: oneshot::Sender<Option<ArticleTask>> },
    ArticleCompleted { task: ArticleResult },
    FileCompleted { file_id: u32 },
    NzbCompleted { nzb_id: u32 },
    MoveToHistory { nzb_id: u32 },
    // ...
}
```

Advantages:

- **No locks** — the coordinator owns all mutable state.
- **No deadlocks** — impossible by construction.
- **Backpressure** — bounded channels prevent queue flooding.
- **Testable** — the coordinator can be driven by a synthetic channel
  in unit tests without spawning real tasks.

### Option C: Hybrid

Use the actor model for write operations but share a read-only
snapshot (`Arc<QueueSnapshot>`) updated periodically (or on every
mutation) for read-heavy consumers like the web UI status endpoint.

### Recommendation

**Start with the actor model (Option B).** It aligns with Rust's
ownership philosophy and eliminates an entire class of concurrency
bugs. If profiling reveals that message-passing overhead is measurable
for hot-path operations (e.g., per-article completion), introduce
targeted optimizations (batching, the hybrid snapshot) at that point.

---

## Snapshot Types

For read-only consumers (API responses, web UI), the coordinator can
produce lightweight snapshots that clone only the data needed:

```rust
#[derive(Debug, Clone)]
pub struct QueueSnapshot {
    pub queue: Vec<NzbSummary>,
    pub history: Vec<HistorySummary>,
    pub download_rate: u64,
    pub remaining_size: u64,
    pub download_paused: bool,
}

#[derive(Debug, Clone)]
pub struct NzbSummary {
    pub id: u32,
    pub name: String,
    pub category: String,
    pub size: u64,
    pub remaining_size: u64,
    pub paused: bool,
    pub priority: Priority,
    pub health: u32,
    pub file_count: u32,
    pub remaining_file_count: u32,
}
```

---

## Relationship to Disk State

All structures above must be serializable for crash recovery. See
[12-disk-state.md](12-disk-state.md) for the persistence format.
`NzbInfo`, `FileInfo`, and `ArticleInfo` implement `serde::Serialize`
and `serde::Deserialize`. The disk-state module writes the full
`DownloadQueue` atomically to avoid corruption on unexpected shutdown.
