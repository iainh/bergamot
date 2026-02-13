# Queue coordinator

The Queue Coordinator is the central orchestrator of bergamot. It owns the download
queue, schedules articles to download connections, tracks progress, detects
file/NZB completion, and enforces speed and quota limits.

## Responsibilities

- **Queue management** — add, remove, reorder, pause/resume NZBs and files.
- **Article scheduling** — select the next article to download and assign it to
  an available connection.
- **Download tracking** — monitor active downloads and handle completions
  (success, retry, failure).
- **File/NZB completion** — detect when all articles for a file or NZB have
  finished and trigger post-processing.
- **Health monitoring** — track article success/failure ratios per file and NZB.
- **Speed & quota** — enforce download rate limits and daily/monthly quotas.

## Architecture

```
                      ┌─────────────────────┐
                      │  RPC / API Layer    │
                      └────────┬────────────┘
                               │ QueueCommand
                               ▼
                 ┌─────────────────────────────┐
                 │      Queue Coordinator       │
                 │                              │
                 │  ┌────────────────────────┐  │
                 │  │    DownloadQueue        │  │
                 │  │  (NZBs, Files, Articles)│  │
                 │  └────────────────────────┘  │
                 │                              │
  completion_rx  │  ┌──────────┐ ┌───────────┐  │  command_rx
  ◄──────────────┤  │ Active   │ │  Article   │  ├──────────────►
                 │  │ Slots    │ │  Cache     │  │
                 │  └──────────┘ └───────────┘  │
                 │                              │
                 │  ┌──────────┐ ┌───────────┐  │
                 │  │  Speed   │ │  Server    │  │
                 │  │  Limiter │ │  Pool      │  │
                 │  └──────────┘ └───────────┘  │
                 └─────────────────────────────┘
                               │
                               ▼
                 ┌─────────────────────────────┐
                 │   Download Tasks (tokio)     │
                 │  connection → decode → done  │
                 └─────────────────────────────┘
```

## Actor model design

The coordinator runs as a single long-lived tokio task. External components
communicate with it via a command channel (MPSC). Download tasks report back
via a separate completion channel.

```rust
use tokio::sync::{mpsc, oneshot};

pub struct QueueCoordinator {
    queue: DownloadQueue,
    active_downloads: HashMap<ArticleId, ActiveDownload>,
    max_connections: usize,

    command_rx: mpsc::Receiver<QueueCommand>,
    completion_rx: mpsc::Receiver<DownloadCompletion>,
    completion_tx: mpsc::Sender<DownloadCompletion>,

    server_pool: ServerPool,
    article_cache: ArticleCache,
    speed_limiter: SpeedLimiter,

    paused: bool,
    shutdown: bool,
}
```

## QueueCommand

All external mutations and queries flow through this enum:

```rust
pub enum QueueCommand {
    AddNzb {
        path: PathBuf,
        category: Option<String>,
        priority: Priority,
        reply: oneshot::Sender<Result<NzbId, QueueError>>,
    },
    RemoveNzb {
        id: NzbId,
        delete_files: bool,
        reply: oneshot::Sender<Result<(), QueueError>>,
    },
    PauseNzb { id: NzbId },
    ResumeNzb { id: NzbId },
    MoveNzb {
        id: NzbId,
        position: MovePosition,
    },
    PauseFile { nzb_id: NzbId, file_index: u32 },
    ResumeFile { nzb_id: NzbId, file_index: u32 },
    PauseAll,
    ResumeAll,
    SetDownloadRate { bytes_per_sec: u64 },
    EditQueue {
        action: EditAction,
        ids: Vec<NzbId>,
        reply: oneshot::Sender<Result<(), QueueError>>,
    },
    GetStatus {
        reply: oneshot::Sender<QueueStatus>,
    },
    GetNzbList {
        reply: oneshot::Sender<Vec<NzbListEntry>>,
    },
    Shutdown,
}
```

## Main loop

The coordinator uses `tokio::select!` to multiplex three event sources:

```rust
impl QueueCoordinator {
    pub async fn run(&mut self) {
        let mut slot_timer = tokio::time::interval(Duration::from_millis(100));

        loop {
            tokio::select! {
                Some(cmd) = self.command_rx.recv() => {
                    self.handle_command(cmd).await;
                }
                Some(completion) = self.completion_rx.recv() => {
                    self.handle_completion(completion).await;
                }
                _ = slot_timer.tick() => {
                    self.fill_download_slots().await;
                }
            }

            if self.shutdown && self.active_downloads.is_empty() {
                self.save_queue().await;
                break;
            }
        }
    }
}
```

The periodic timer ensures download slots are filled even when no completions
or commands arrive (e.g., after un-pausing).

## Article selection algorithm

When filling download slots, the coordinator walks the queue in priority order
and picks the first eligible article:

```rust
impl QueueCoordinator {
    fn next_article(&self) -> Option<(NzbId, u32, u32)> {
        for nzb in self.queue.nzbs_by_priority() {
            if nzb.paused {
                continue;
            }
            for file in nzb.files_by_priority() {
                if file.paused {
                    continue;
                }
                let active_count = self.active_articles_for_file(nzb.id, file.index);
                if active_count >= self.max_articles_per_file {
                    continue;
                }
                for (seg_idx, segment) in file.segments.iter().enumerate() {
                    if segment.status == SegmentStatus::Undefined {
                        return Some((nzb.id, file.index, seg_idx as u32));
                    }
                }
            }
        }
        None
    }
}
```

## Priority ordering

Articles are selected according to a multi-level priority scheme:

1. **NZB priority** — user-assigned priority (Force, Very High, High, Normal,
   Low, Very Low).
2. **File type priority** — within an NZB:
   - PAR2 main file (lowest article count, needed for repair detection)
   - Data files (the actual content)
   - PAR2 repair volumes (downloaded on-demand when health is low)
3. **File order** — files within the same type are processed in NZB order.
4. **Force priority** — `Priority::Force` bypasses pause state and is always
   scheduled first.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    VeryLow = -100,
    Low = -50,
    Normal = 0,
    High = 50,
    VeryHigh = 100,
    Force = 900,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FileTypePriority {
    Par2Main = 0,
    Data = 1,
    Par2Repair = 2,
}
```

## Download slot management

```rust
impl QueueCoordinator {
    async fn fill_download_slots(&mut self) {
        while self.active_downloads.len() < self.max_connections {
            if self.paused {
                break;
            }
            if !self.speed_limiter.has_quota() {
                break;
            }

            let Some((nzb_id, file_idx, seg_idx)) = self.next_article() else {
                break;
            };

            let Some(connection) = self.server_pool.acquire().await else {
                break;
            };

            let article_id = ArticleId { nzb_id, file_idx, seg_idx };
            let message_id = self.queue.message_id(nzb_id, file_idx, seg_idx)
                .expect("article must exist");

            self.queue.set_segment_status(
                nzb_id, file_idx, seg_idx, SegmentStatus::Downloading,
            );

            let completion_tx = self.completion_tx.clone();
            let limiter = self.speed_limiter.clone();

            tokio::spawn(async move {
                let result = download_article(
                    connection, &message_id, &limiter,
                ).await;
                let _ = completion_tx.send(DownloadCompletion {
                    article_id,
                    result,
                }).await;
            });

            self.active_downloads.insert(article_id, ActiveDownload {
                started: Instant::now(),
            });
        }
    }
}
```

## Completion handling

```rust
pub struct DownloadCompletion {
    pub article_id: ArticleId,
    pub result: ArticleResult,
}

pub enum ArticleResult {
    Success {
        segment: DecodedSegment,
    },
    Retry {
        reason: RetryReason,
        server_id: ServerId,
    },
    Failed {
        error: DownloadError,
    },
}

impl QueueCoordinator {
    async fn handle_completion(&mut self, completion: DownloadCompletion) {
        self.active_downloads.remove(&completion.article_id);
        let DownloadCompletion { article_id, result } = completion;

        match result {
            ArticleResult::Success { segment } => {
                self.queue.set_segment_status(
                    article_id.nzb_id,
                    article_id.file_idx,
                    article_id.seg_idx,
                    SegmentStatus::Completed,
                );
                self.article_cache.store(
                    article_id.nzb_id,
                    article_id.file_idx,
                    segment,
                );
                self.check_file_completion(
                    article_id.nzb_id,
                    article_id.file_idx,
                ).await;
            }
            ArticleResult::Retry { reason, server_id } => {
                self.queue.mark_server_failed(
                    article_id.nzb_id,
                    article_id.file_idx,
                    article_id.seg_idx,
                    server_id,
                );
                self.queue.set_segment_status(
                    article_id.nzb_id,
                    article_id.file_idx,
                    article_id.seg_idx,
                    SegmentStatus::Undefined,
                );
            }
            ArticleResult::Failed { error } => {
                self.queue.set_segment_status(
                    article_id.nzb_id,
                    article_id.file_idx,
                    article_id.seg_idx,
                    SegmentStatus::Failed,
                );
                self.update_health(article_id.nzb_id).await;
            }
        }
    }
}
```

## File & NZB completion detection

```rust
impl QueueCoordinator {
    async fn check_file_completion(&mut self, nzb_id: NzbId, file_idx: u32) {
        let file = &self.queue.file(nzb_id, file_idx);
        let all_done = file.segments.iter().all(|s| {
            matches!(s.status, SegmentStatus::Completed | SegmentStatus::Failed)
        });

        if !all_done {
            return;
        }

        // Flush cached segments to disk and finalize.
        if let Err(e) = self.finalize_file(nzb_id, file_idx).await {
            tracing::error!(nzb_id = ?nzb_id, file_idx, "file completion failed: {e}");
        }

        // Check if entire NZB is now complete.
        let nzb = &self.queue.nzb(nzb_id);
        let nzb_done = nzb.files.iter().all(|f| f.completed);
        if nzb_done {
            self.complete_nzb(nzb_id).await;
        }
    }

    async fn complete_nzb(&mut self, nzb_id: NzbId) {
        tracing::info!(nzb_id = ?nzb_id, "NZB download complete, triggering post-processing");
        self.queue.set_nzb_status(nzb_id, NzbStatus::PostProcessing);
        // Send to post-processing pipeline (par-check, unpack, etc.)
    }
}
```

## Health monitoring

Health is tracked as a permille value (0–1000) representing the ratio of
successfully downloaded articles to total articles.

```rust
impl DownloadQueue {
    /// Returns file health as permille (0–1000).
    pub fn health(&self, nzb_id: NzbId, file_idx: u32) -> u16 {
        let file = &self.file(nzb_id, file_idx);
        let total = file.segments.len() as u64;
        if total == 0 {
            return 1000;
        }
        let success = file.segments.iter()
            .filter(|s| s.status == SegmentStatus::Completed)
            .count() as u64;
        ((success * 1000) / total) as u16
    }

    /// Returns critical health threshold: the minimum permille at which
    /// the file can still be repaired with available PAR2 data.
    pub fn critical_health(&self, nzb_id: NzbId) -> u16 {
        let nzb = &self.nzb(nzb_id);
        let total_data_segments: u64 = nzb.data_files()
            .map(|f| f.segments.len() as u64)
            .sum();
        let par2_repair_segments: u64 = nzb.par2_repair_files()
            .map(|f| f.segments.len() as u64)
            .sum();
        let required = total_data_segments.saturating_sub(par2_repair_segments);
        if total_data_segments == 0 {
            return 1000;
        }
        ((required * 1000) / total_data_segments) as u16
    }
}
```

### HealthCheck modes

| Mode     | Behaviour                                                             |
|----------|-----------------------------------------------------------------------|
| `Park`   | Pause the NZB when health drops below critical; wait for manual action.|
| `Delete` | Automatically delete the NZB and its files when health is critical.   |
| `None`   | Continue downloading regardless of health.                            |

```rust
#[derive(Debug, Clone, Copy)]
pub enum HealthCheck {
    Park,
    Delete,
    None,
}
```

## Queue edit operations

Batch editing allows the API layer to apply operations to one or more NZBs in a
single command:

```rust
pub enum EditAction {
    Move(MovePosition),
    Pause,
    Resume,
    Delete { delete_files: bool },
    SetPriority(Priority),
    SetCategory(String),
    SetParameter { key: String, value: String },
    Merge { target_id: NzbId },
    Split { file_indices: Vec<u32> },
    SetName(String),
    SetDupeKey(String),
    SetDupeScore(i32),
    SetDupeMode(DupeMode),
}

#[derive(Debug, Clone, Copy)]
pub enum MovePosition {
    Top,
    Bottom,
    Up(u32),
    Down(u32),
    Before(NzbId),
    After(NzbId),
}

#[derive(Debug, Clone, Copy)]
pub enum DupeMode {
    Score,
    All,
    Force,
}
```

## State persistence

The queue state is persisted to disk so that downloads survive restarts.

- **Periodic saves** — a background timer triggers a save every N seconds
  (default: 30) if the queue is dirty.
- **Save triggers** — certain operations force an immediate save: NZB added,
  NZB removed, NZB completed, shutdown.

```rust
impl QueueCoordinator {
    async fn maybe_save_queue(&mut self) {
        if self.queue.is_dirty() {
            self.save_queue().await;
        }
    }

    async fn save_queue(&mut self) {
        let snapshot = self.queue.serialize();
        let path = self.data_dir.join("queue.json");
        let tmp_path = self.data_dir.join("queue.json.tmp");

        if let Err(e) = tokio::fs::write(&tmp_path, &snapshot).await {
            tracing::error!("failed to write queue snapshot: {e}");
            return;
        }
        if let Err(e) = tokio::fs::rename(&tmp_path, &path).await {
            tracing::error!("failed to rename queue snapshot: {e}");
        }
        self.queue.clear_dirty();
    }
}
```

The write-to-temp-then-rename pattern ensures atomic updates — a crash during
the write will not corrupt the existing queue file.
