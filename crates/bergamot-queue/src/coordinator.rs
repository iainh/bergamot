use std::collections::HashMap;
use std::time::Instant;

use tokio::sync::{mpsc, oneshot, watch};

use bergamot_core::models::{
    ArticleStatus, DownloadQueue, FileInfo, HistoryInfo, HistoryKind, NzbInfo, PostStrategy,
    Priority,
};

use crate::command::{EditAction, MovePosition, QueueCommand};
use crate::error::QueueError;
use crate::status::{
    FileArticleSnapshot, HistoryListEntry, NzbCompletionNotice, NzbListEntry, NzbSnapshotEntry,
    QueueSnapshot, QueueStatus, SegmentStatus,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArticleId {
    pub nzb_id: u32,
    pub file_idx: u32,
    pub seg_idx: u32,
}

#[derive(Debug, Clone)]
pub struct ArticleAssignment {
    pub article_id: ArticleId,
    pub message_id: String,
    pub groups: Vec<String>,
    pub output_filename: String,
    pub expected_size: u64,
    /// Which server this article is assigned to by the weighted fair queuing scheduler.
    /// When `None`, the download worker should use the default server pool behavior.
    pub server_id: Option<u32>,
}

#[derive(Debug)]
struct ActiveDownload {
    started: Instant,
    /// Server this article was assigned to, for throughput tracking on completion.
    server_id: Option<u32>,
    /// Expected article size in bytes, needed to update scheduler on completion.
    expected_size: u64,
}

#[derive(Debug, Clone)]
pub struct QueueHandle {
    command_tx: mpsc::Sender<QueueCommand>,
}

impl QueueHandle {
    pub async fn add_nzb(
        &self,
        path: std::path::PathBuf,
        category: Option<String>,
        priority: Priority,
    ) -> Result<u32, QueueError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let cmd = QueueCommand::AddNzb {
            path,
            category,
            priority,
            reply: reply_tx,
        };
        self.command_tx
            .send(cmd)
            .await
            .map_err(|_| QueueError::Shutdown)?;
        reply_rx.await.map_err(|_| QueueError::Shutdown)?
    }

    pub async fn get_status(&self) -> Result<QueueStatus, QueueError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let cmd = QueueCommand::GetStatus { reply: reply_tx };
        self.command_tx
            .send(cmd)
            .await
            .map_err(|_| QueueError::Shutdown)?;
        reply_rx.await.map_err(|_| QueueError::Shutdown)
    }

    pub async fn report_download(
        &self,
        result: crate::command::DownloadResult,
    ) -> Result<(), QueueError> {
        self.command_tx
            .send(QueueCommand::DownloadComplete(result))
            .await
            .map_err(|_| QueueError::Shutdown)
    }

    pub async fn pause_all(&self) -> Result<(), QueueError> {
        self.command_tx
            .send(QueueCommand::PauseAll)
            .await
            .map_err(|_| QueueError::Shutdown)
    }

    pub async fn resume_all(&self) -> Result<(), QueueError> {
        self.command_tx
            .send(QueueCommand::ResumeAll)
            .await
            .map_err(|_| QueueError::Shutdown)
    }

    pub async fn set_download_rate(&self, bytes_per_sec: u64) -> Result<(), QueueError> {
        self.command_tx
            .send(QueueCommand::SetDownloadRate { bytes_per_sec })
            .await
            .map_err(|_| QueueError::Shutdown)
    }

    pub async fn get_queue_snapshot(&self) -> Result<QueueSnapshot, QueueError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.command_tx
            .send(QueueCommand::GetQueueSnapshot { reply: reply_tx })
            .await
            .map_err(|_| QueueError::Shutdown)?;
        reply_rx.await.map_err(|_| QueueError::Shutdown)
    }

    pub async fn get_nzb_list(&self) -> Result<Vec<NzbListEntry>, QueueError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.command_tx
            .send(QueueCommand::GetNzbList { reply: reply_tx })
            .await
            .map_err(|_| QueueError::Shutdown)?;
        reply_rx.await.map_err(|_| QueueError::Shutdown)
    }

    pub async fn edit_queue(&self, action: EditAction, ids: Vec<u32>) -> Result<(), QueueError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.command_tx
            .send(QueueCommand::EditQueue {
                action,
                ids,
                reply: reply_tx,
            })
            .await
            .map_err(|_| QueueError::Shutdown)?;
        reply_rx.await.map_err(|_| QueueError::Shutdown)?
    }

    pub async fn par_unpause(&self, nzb_id: u32) -> Result<(), QueueError> {
        self.command_tx
            .send(QueueCommand::ParUnpause { nzb_id })
            .await
            .map_err(|_| QueueError::Shutdown)
    }

    pub async fn get_file_list(
        &self,
        nzb_id: u32,
    ) -> Result<Vec<crate::status::FileListEntry>, QueueError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.command_tx
            .send(QueueCommand::GetFileList {
                nzb_id,
                reply: reply_tx,
            })
            .await
            .map_err(|_| QueueError::Shutdown)?;
        reply_rx.await.map_err(|_| QueueError::Shutdown)?
    }

    pub async fn get_history(&self) -> Result<Vec<crate::status::HistoryListEntry>, QueueError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.command_tx
            .send(QueueCommand::GetHistory { reply: reply_tx })
            .await
            .map_err(|_| QueueError::Shutdown)?;
        reply_rx.await.map_err(|_| QueueError::Shutdown)
    }

    pub async fn history_return(&self, history_id: u32) -> Result<u32, QueueError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.command_tx
            .send(QueueCommand::HistoryReturn {
                history_id,
                reply: reply_tx,
            })
            .await
            .map_err(|_| QueueError::Shutdown)?;
        reply_rx.await.map_err(|_| QueueError::Shutdown)?
    }

    pub async fn history_redownload(&self, history_id: u32) -> Result<u32, QueueError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.command_tx
            .send(QueueCommand::HistoryRedownload {
                history_id,
                reply: reply_tx,
            })
            .await
            .map_err(|_| QueueError::Shutdown)?;
        reply_rx.await.map_err(|_| QueueError::Shutdown)?
    }

    pub async fn history_mark(
        &self,
        history_id: u32,
        mark: bergamot_core::models::MarkStatus,
    ) -> Result<(), QueueError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.command_tx
            .send(QueueCommand::HistoryMark {
                history_id,
                mark,
                reply: reply_tx,
            })
            .await
            .map_err(|_| QueueError::Shutdown)?;
        reply_rx.await.map_err(|_| QueueError::Shutdown)?
    }

    pub async fn history_delete(&self, history_id: u32) -> Result<(), QueueError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.command_tx
            .send(QueueCommand::HistoryDelete {
                history_id,
                reply: reply_tx,
            })
            .await
            .map_err(|_| QueueError::Shutdown)?;
        reply_rx.await.map_err(|_| QueueError::Shutdown)?
    }

    pub async fn update_post_status(
        &self,
        nzb_id: u32,
        par_status: Option<bergamot_core::models::ParStatus>,
        unpack_status: Option<bergamot_core::models::UnpackStatus>,
        move_status: Option<bergamot_core::models::MoveStatus>,
    ) -> Result<(), QueueError> {
        self.command_tx
            .send(QueueCommand::UpdatePostStatus {
                nzb_id,
                par_status,
                unpack_status,
                move_status,
            })
            .await
            .map_err(|_| QueueError::Shutdown)
    }

    pub async fn update_post_stage(
        &self,
        nzb_id: u32,
        stage: bergamot_core::models::PostStage,
    ) -> Result<(), QueueError> {
        self.command_tx
            .send(QueueCommand::UpdatePostStage { nzb_id, stage })
            .await
            .map_err(|_| QueueError::Shutdown)
    }

    pub async fn update_post_progress(&self, nzb_id: u32, progress: u32) -> Result<(), QueueError> {
        self.command_tx
            .send(QueueCommand::UpdatePostProgress { nzb_id, progress })
            .await
            .map_err(|_| QueueError::Shutdown)
    }

    pub async fn finish_post_processing(
        &self,
        nzb_id: u32,
        par_status: bergamot_core::models::ParStatus,
        unpack_status: bergamot_core::models::UnpackStatus,
        move_status: bergamot_core::models::MoveStatus,
        timings: crate::command::PostProcessTimings,
    ) -> Result<(), QueueError> {
        self.command_tx
            .send(QueueCommand::FinishPostProcessing {
                nzb_id,
                par_status,
                unpack_status,
                move_status,
                timings,
            })
            .await
            .map_err(|_| QueueError::Shutdown)
    }

    pub async fn set_strategy(
        &self,
        strategy: bergamot_core::models::PostStrategy,
    ) -> Result<(), QueueError> {
        self.command_tx
            .send(QueueCommand::SetStrategy { strategy })
            .await
            .map_err(|_| QueueError::Shutdown)
    }

    pub async fn get_all_file_article_states(
        &self,
    ) -> Result<Vec<crate::status::FileArticleSnapshot>, QueueError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.command_tx
            .send(QueueCommand::GetAllFileArticleStates { reply: reply_tx })
            .await
            .map_err(|_| QueueError::Shutdown)?;
        reply_rx.await.map_err(|_| QueueError::Shutdown)
    }

    pub async fn get_scheduler_stats(
        &self,
    ) -> Result<Vec<crate::status::SchedulerSlotStats>, QueueError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.command_tx
            .send(QueueCommand::GetSchedulerStats { reply: reply_tx })
            .await
            .map_err(|_| QueueError::Shutdown)?;
        reply_rx.await.map_err(|_| QueueError::Shutdown)
    }

    pub async fn shutdown(&self) -> Result<(), QueueError> {
        self.command_tx
            .send(QueueCommand::Shutdown)
            .await
            .map_err(|_| QueueError::Shutdown)
    }
}

pub struct QueueCoordinator {
    queue: DownloadQueue,
    active_downloads: HashMap<ArticleId, ActiveDownload>,
    active_per_file: HashMap<(u32, u32), usize>,
    max_connections: usize,
    max_articles_per_file: usize,
    download_rate: u64,
    paused: bool,
    shutdown: bool,
    strategy: PostStrategy,
    inter_dir: std::path::PathBuf,
    dest_dir: std::path::PathBuf,
    command_rx: mpsc::Receiver<QueueCommand>,
    assignment_tx: mpsc::Sender<ArticleAssignment>,
    rate_watch_tx: watch::Sender<u64>,
    completion_tx: Option<mpsc::Sender<NzbCompletionNotice>>,
    session_downloaded: u64,
    /// Optional server scheduler for weighted fair queuing.
    /// When present, articles are assigned to specific servers based on
    /// their throughput-weighted capacity. When absent, the download worker
    /// falls back to the existing ServerPool behavior (try all servers).
    server_scheduler: Option<bergamot_nntp::ServerScheduler>,
}

impl QueueCoordinator {
    pub fn new(
        max_connections: usize,
        max_articles_per_file: usize,
        inter_dir: std::path::PathBuf,
        dest_dir: std::path::PathBuf,
    ) -> (
        Self,
        QueueHandle,
        mpsc::Receiver<ArticleAssignment>,
        watch::Receiver<u64>,
    ) {
        let (command_tx, command_rx) = mpsc::channel(64);
        let channel_capacity = max_connections.max(64) * 4;
        let (assignment_tx, assignment_rx) = mpsc::channel(channel_capacity);
        let (rate_watch_tx, rate_watch_rx) = watch::channel(0u64);
        let handle = QueueHandle { command_tx };
        let coordinator = Self {
            completion_tx: None,
            queue: DownloadQueue {
                queue: Vec::new(),
                history: Vec::new(),
                next_nzb_id: 1,
                next_file_id: 1,
            },
            active_downloads: HashMap::new(),
            active_per_file: HashMap::new(),
            max_connections,
            max_articles_per_file,
            download_rate: 0,
            paused: false,
            shutdown: false,
            strategy: PostStrategy::default(),
            inter_dir,
            dest_dir,
            command_rx,
            assignment_tx,
            rate_watch_tx,
            session_downloaded: 0,
            server_scheduler: None,
        };
        (coordinator, handle, assignment_rx, rate_watch_rx)
    }

    pub fn with_completion_tx(mut self, tx: mpsc::Sender<NzbCompletionNotice>) -> Self {
        self.completion_tx = Some(tx);
        self
    }

    /// Attach a server scheduler for weighted fair queuing.
    /// When set, the coordinator assigns articles to specific servers based
    /// on their throughput capacity, rather than letting the download worker
    /// try all servers.
    pub fn with_server_scheduler(mut self, scheduler: bergamot_nntp::ServerScheduler) -> Self {
        self.server_scheduler = Some(scheduler);
        self
    }

    /// Main coordinator loop.
    ///
    /// Uses `tokio::select!` with a periodic refill tick to prevent pipeline
    /// bubbles. Without this, slots would only be filled when a command
    /// (e.g., DownloadComplete) arrives, leaving connections idle between
    /// article completions. The 50ms tick ensures the scheduler recovers
    /// from missed wakeups and channel backpressure without waiting for
    /// the next external event.
    pub async fn run(&mut self) {
        self.try_fill_download_slots();

        let mut refill_tick = tokio::time::interval(std::time::Duration::from_millis(50));
        refill_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                biased;

                Some(cmd) = self.command_rx.recv() => {
                    self.handle_command(cmd).await;
                    self.try_fill_download_slots();
                }

                _ = refill_tick.tick() => {
                    self.try_fill_download_slots();
                }
            }

            if self.shutdown && self.active_downloads.is_empty() {
                break;
            }
        }
    }

    fn try_fill_download_slots(&mut self) {
        if self.paused || self.shutdown {
            return;
        }

        // Determine the effective connection limit. When a server scheduler
        // is present, it tracks per-server capacity; the global limit is the
        // sum of all server connection limits. Without a scheduler, fall back
        // to the configured max_connections.
        let max_slots = if let Some(ref sched) = self.server_scheduler {
            sched.total_max_connections() as usize
        } else {
            self.max_connections
        };

        while self.active_downloads.len() < max_slots {
            // Check if the server scheduler has capacity before looking for
            // the next article. This avoids wasting time selecting an article
            // when all server queues are full.
            if let Some(ref sched) = self.server_scheduler
                && sched.total_available_capacity() == 0
            {
                break;
            }

            let Some((article_id, mut assignment)) = self.next_assignment() else {
                break;
            };

            // If a server scheduler is configured, use WFQ to select the
            // best server for this article based on throughput-weighted
            // pending work ratios (see ServerScheduler::select_server).
            if let Some(ref mut sched) = self.server_scheduler
                && let Some(sid) = sched.select_server(assignment.expected_size)
            {
                assignment.server_id = Some(sid);
                sched.assign_to_server(sid, assignment.expected_size);
            }

            let server_id = assignment.server_id;
            let expected_size = assignment.expected_size;
            match self.assignment_tx.try_send(assignment) {
                Ok(()) => {
                    tracing::debug!(
                        nzb_id = article_id.nzb_id,
                        file_idx = article_id.file_idx,
                        seg_idx = article_id.seg_idx,
                        ?server_id,
                        active = self.active_downloads.len() + 1,
                        max = max_slots,
                        "dispatched article"
                    );
                    self.mark_segment_status(article_id, SegmentStatus::Downloading);
                    self.increment_active_per_file((article_id.nzb_id, article_id.file_idx));
                    self.active_downloads.insert(
                        article_id,
                        ActiveDownload {
                            started: Instant::now(),
                            server_id,
                            expected_size,
                        },
                    );
                }
                Err(mpsc::error::TrySendError::Full(rejected)) => {
                    // Roll back scheduler state if we couldn't send.
                    if let Some(ref mut sched) = self.server_scheduler
                        && let Some(sid) = rejected.server_id
                    {
                        sched.complete_on_server(
                            sid,
                            rejected.expected_size,
                            std::time::Duration::ZERO,
                            false,
                        );
                    }
                    tracing::debug!("assignment channel full");
                    break;
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    tracing::debug!("assignment channel closed");
                    break;
                }
            }
        }

        if self.active_downloads.len() < self.max_connections {
            tracing::trace!(
                active = self.active_downloads.len(),
                max = self.max_connections,
                "could not fill all download slots"
            );
        }
    }

    fn next_assignment(&self) -> Option<(ArticleId, ArticleAssignment)> {
        let article_id = self.next_article()?;
        let nzb = self
            .queue
            .queue
            .iter()
            .find(|n| n.id == article_id.nzb_id)?;
        let file = nzb.files.get(article_id.file_idx as usize)?;
        let segment = file.articles.get(article_id.seg_idx as usize)?;
        Some((
            article_id,
            ArticleAssignment {
                article_id,
                message_id: segment.message_id.clone(),
                groups: file.groups.clone(),
                output_filename: file.output_filename.clone(),
                expected_size: segment.size,
                server_id: None,
            },
        ))
    }

    fn next_article(&self) -> Option<ArticleId> {
        match self.strategy {
            PostStrategy::Sequential => self.next_article_sequential(),
            PostStrategy::Rocket => self.next_article_rocket(),
            PostStrategy::Balanced => self.next_article_balanced(),
            PostStrategy::Aggressive => self.next_article_aggressive(),
        }
    }

    fn next_article_sequential(&self) -> Option<ArticleId> {
        self.next_article_filtered(|_| true)
    }

    fn next_article_rocket(&self) -> Option<ArticleId> {
        if let Some(id) = self.next_article_filtered(|file| !is_par_file(&file.filename)) {
            return Some(id);
        }
        self.next_article_filtered(|_| true)
    }

    fn next_article_balanced(&self) -> Option<ArticleId> {
        if let Some(id) = self.next_article_filtered(|file| !is_par_file(&file.filename)) {
            return Some(id);
        }
        self.next_article_filtered(|file| is_par_file(&file.filename))
    }

    fn next_article_aggressive(&self) -> Option<ArticleId> {
        if let Some(id) = self.next_article_filtered(|file| is_par_file(&file.filename)) {
            return Some(id);
        }
        self.next_article_filtered(|file| !is_par_file(&file.filename))
    }

    fn next_article_filtered<F>(&self, file_filter: F) -> Option<ArticleId>
    where
        F: Fn(&FileInfo) -> bool,
    {
        // Adaptive per-file cap: when only one file is runnable (common for
        // large rar sets), allow it to use all available connection slots.
        // This prevents artificial throttling when there's no contention
        // between files. When multiple files are runnable, the configured
        // max_articles_per_file limit ensures fair distribution.
        let runnable_files = self.count_runnable_files(&file_filter);
        let effective_per_file_cap = if runnable_files <= 1 {
            self.max_connections
        } else {
            self.max_articles_per_file
        };

        let mut nzbs = self.queue.queue.iter().collect::<Vec<_>>();
        nzbs.sort_by_key(|nzb| nzb.priority);
        for nzb in nzbs.iter().rev() {
            if nzb.post_info.is_some() {
                continue;
            }
            if nzb.paused && nzb.priority != Priority::Force {
                continue;
            }
            for (file_idx, file) in nzb.files.iter().enumerate() {
                if file.paused && nzb.priority != Priority::Force {
                    continue;
                }
                if !file_filter(file) {
                    continue;
                }
                let active_count = self.active_articles_for_file(nzb.id, file_idx as u32);
                if active_count >= effective_per_file_cap {
                    continue;
                }
                for (seg_idx, segment) in file.articles.iter().enumerate() {
                    if segment.status == bergamot_core::models::ArticleStatus::Undefined {
                        return Some(ArticleId {
                            nzb_id: nzb.id,
                            file_idx: file_idx as u32,
                            seg_idx: seg_idx as u32,
                        });
                    }
                }
            }
        }
        None
    }

    fn active_articles_for_file(&self, nzb_id: u32, file_idx: u32) -> usize {
        self.active_per_file_count((nzb_id, file_idx))
    }

    fn active_per_file_count(&self, key: (u32, u32)) -> usize {
        self.active_per_file.get(&key).copied().unwrap_or(0)
    }

    fn increment_active_per_file(&mut self, key: (u32, u32)) {
        *self.active_per_file.entry(key).or_insert(0) += 1;
    }

    fn decrement_active_per_file(&mut self, key: (u32, u32)) {
        if let Some(count) = self.active_per_file.get_mut(&key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.active_per_file.remove(&key);
            }
        }
    }

    /// Count files that have remaining downloadable segments and pass the filter.
    /// Used to decide whether to relax the per-file concurrency cap.
    fn count_runnable_files<F>(&self, file_filter: &F) -> usize
    where
        F: Fn(&FileInfo) -> bool,
    {
        let mut count = 0;
        for nzb in &self.queue.queue {
            if nzb.post_info.is_some() {
                continue;
            }
            if nzb.paused && nzb.priority != Priority::Force {
                continue;
            }
            for file in &nzb.files {
                if file.paused && nzb.priority != Priority::Force {
                    continue;
                }
                if !file_filter(file) {
                    continue;
                }
                if file
                    .articles
                    .iter()
                    .any(|a| a.status == bergamot_core::models::ArticleStatus::Undefined)
                {
                    count += 1;
                }
            }
        }
        count
    }

    fn mark_segment_status(&mut self, article_id: ArticleId, status: SegmentStatus) {
        if let Some(nzb) = self
            .queue
            .queue
            .iter_mut()
            .find(|nzb| nzb.id == article_id.nzb_id)
            && let Some(file) = nzb.files.get_mut(article_id.file_idx as usize)
            && let Some(segment) = file.articles.get_mut(article_id.seg_idx as usize)
        {
            segment.status = match status {
                SegmentStatus::Undefined => bergamot_core::models::ArticleStatus::Undefined,
                SegmentStatus::Downloading => bergamot_core::models::ArticleStatus::Running,
                SegmentStatus::Completed => bergamot_core::models::ArticleStatus::Finished,
                SegmentStatus::Failed => bergamot_core::models::ArticleStatus::Failed,
            };
        }
    }

    async fn handle_command(&mut self, cmd: QueueCommand) {
        match cmd {
            QueueCommand::AddNzb {
                path,
                category,
                priority,
                reply,
            } => {
                let result = self.ingest_nzb(&path, category, priority);
                let _ = reply.send(result);
            }
            QueueCommand::RemoveNzb { id, reply, .. } => {
                let len_before = self.queue.queue.len();
                self.queue.queue.retain(|nzb| nzb.id != id);
                if self.queue.queue.len() == len_before {
                    let _ = reply.send(Err(QueueError::NzbNotFound(id)));
                } else {
                    let _ = reply.send(Ok(()));
                }
            }
            QueueCommand::PauseNzb { id } => {
                tracing::debug!(nzb_id = id, "pausing nzb");
                if let Some(nzb) = self.queue.queue.iter_mut().find(|nzb| nzb.id == id) {
                    nzb.paused = true;
                }
            }
            QueueCommand::ResumeNzb { id } => {
                tracing::debug!(nzb_id = id, "resuming nzb");
                if let Some(nzb) = self.queue.queue.iter_mut().find(|nzb| nzb.id == id) {
                    nzb.paused = false;
                }
            }
            QueueCommand::MoveNzb { id, position } => {
                let _ = self.move_nzb(id, position);
            }
            QueueCommand::PauseFile { nzb_id, file_index } => {
                if let Some(file) = self.file_mut(nzb_id, file_index) {
                    file.paused = true;
                }
            }
            QueueCommand::ResumeFile { nzb_id, file_index } => {
                if let Some(file) = self.file_mut(nzb_id, file_index) {
                    file.paused = false;
                }
            }
            QueueCommand::PauseAll => {
                tracing::info!("pausing all downloads");
                self.paused = true;
            }
            QueueCommand::ResumeAll => {
                tracing::info!("resuming all downloads");
                self.paused = false;
                for nzb in &mut self.queue.queue {
                    nzb.paused = false;
                }
            }
            QueueCommand::SetDownloadRate { bytes_per_sec } => {
                self.download_rate = bytes_per_sec;
                let _ = self.rate_watch_tx.send(bytes_per_sec);
            }
            QueueCommand::EditQueue { action, ids, reply } => {
                let result = self.apply_edit(action, ids);
                let _ = reply.send(result);
            }
            QueueCommand::GetStatus { reply } => {
                let remaining_size: u64 =
                    self.queue.queue.iter().map(|nzb| nzb.remaining_size).sum();
                let downloaded_size: u64 = self.session_downloaded;
                let status = QueueStatus {
                    queued: self.queue.queue.len(),
                    paused: self.paused,
                    download_rate: self.download_rate,
                    remaining_size,
                    downloaded_size,
                };
                let _ = reply.send(status);
            }
            QueueCommand::GetNzbList { reply } => {
                let list = self
                    .queue
                    .queue
                    .iter()
                    .map(|nzb| NzbListEntry {
                        id: nzb.id,
                        name: nzb.name.clone(),
                        priority: nzb.priority,
                    })
                    .collect();
                let _ = reply.send(list);
            }
            QueueCommand::GetQueueSnapshot { reply } => {
                let snapshot = self.build_snapshot();
                let _ = reply.send(snapshot);
            }
            QueueCommand::DownloadComplete(result) => {
                self.handle_download_complete(result);
            }
            QueueCommand::ParUnpause { nzb_id } => {
                self.unpause_par_files(nzb_id);
            }
            QueueCommand::GetFileList { nzb_id, reply } => {
                let result = self.build_file_list(nzb_id);
                let _ = reply.send(result);
            }
            QueueCommand::GetHistory { reply } => {
                let list = self.build_history_list();
                let _ = reply.send(list);
            }
            QueueCommand::HistoryReturn { history_id, reply } => {
                let result = self.history_return_to_queue(history_id);
                let _ = reply.send(result);
            }
            QueueCommand::HistoryRedownload { history_id, reply } => {
                let result = self.history_redownload(history_id);
                let _ = reply.send(result);
            }
            QueueCommand::HistoryMark {
                history_id,
                mark,
                reply,
            } => {
                let result = self.history_mark(history_id, mark);
                let _ = reply.send(result);
            }
            QueueCommand::HistoryDelete { history_id, reply } => {
                let result = self.history_delete(history_id);
                let _ = reply.send(result);
            }
            QueueCommand::UpdatePostStatus {
                nzb_id,
                par_status,
                unpack_status,
                move_status,
            } => {
                self.update_post_status(nzb_id, par_status, unpack_status, move_status);
            }
            QueueCommand::UpdatePostStage { nzb_id, stage } => {
                self.update_post_stage(nzb_id, stage);
            }
            QueueCommand::UpdatePostProgress { nzb_id, progress } => {
                self.update_post_progress(nzb_id, progress);
            }
            QueueCommand::FinishPostProcessing {
                nzb_id,
                par_status,
                unpack_status,
                move_status,
                timings,
            } => {
                self.finish_post_processing(
                    nzb_id,
                    par_status,
                    unpack_status,
                    move_status,
                    timings,
                );
            }
            QueueCommand::GetAllFileArticleStates { reply } => {
                let states = self.build_all_file_article_states();
                let _ = reply.send(states);
            }
            QueueCommand::SetStrategy { strategy } => {
                self.strategy = strategy;
            }
            QueueCommand::GetSchedulerStats { reply } => {
                let stats = self.build_scheduler_stats();
                let _ = reply.send(stats);
            }
            QueueCommand::Shutdown => {
                self.shutdown = true;
                self.active_downloads.clear();
                self.active_per_file.clear();
            }
        }
    }

    fn handle_download_complete(&mut self, result: crate::command::DownloadResult) {
        let article_id = &result.article_id;
        self.decrement_active_per_file((article_id.nzb_id, article_id.file_idx));
        let download_info = self.active_downloads.remove(&result.article_id);
        tracing::debug!(
            nzb_id = result.article_id.nzb_id,
            file_idx = result.article_id.file_idx,
            seg_idx = result.article_id.seg_idx,
            active_downloads = self.active_downloads.len(),
            "removed from active downloads"
        );

        // Feed completion data back to the server scheduler for EWMA
        // throughput estimation. This allows the WFQ algorithm to adapt
        // server weights based on observed transfer speeds.
        if let Some(ref mut sched) = self.server_scheduler {
            let server_id = result
                .server_id
                .or_else(|| download_info.as_ref().and_then(|d| d.server_id));
            let article_size = download_info.as_ref().map(|d| d.expected_size).unwrap_or(0);
            let elapsed = result
                .elapsed
                .or_else(|| download_info.as_ref().map(|d| d.started.elapsed()))
                .unwrap_or_default();
            let success = matches!(
                result.outcome,
                crate::command::DownloadOutcome::Success { .. }
            );
            if let Some(sid) = server_id {
                sched.complete_on_server(sid, article_size, elapsed, success);
            }
        }

        let nzb_id = result.article_id.nzb_id;
        let file_idx = result.article_id.file_idx as usize;
        let seg_idx = result.article_id.seg_idx as usize;

        if let Some(nzb) = self.queue.queue.iter_mut().find(|n| n.id == nzb_id) {
            let article_size = nzb
                .files
                .get(file_idx)
                .and_then(|f| f.articles.get(seg_idx))
                .map(|a| a.size)
                .unwrap_or(0);

            let already_terminal = nzb
                .files
                .get(file_idx)
                .and_then(|f| f.articles.get(seg_idx))
                .is_some_and(|a| {
                    a.status == ArticleStatus::Finished || a.status == ArticleStatus::Failed
                });

            if already_terminal {
                tracing::debug!(
                    nzb_id,
                    file_idx,
                    seg_idx,
                    "segment already terminal, ignoring"
                );
                return;
            }

            let file_is_par = nzb
                .files
                .get(file_idx)
                .is_some_and(|f| is_par_file(&f.filename));

            match result.outcome {
                crate::command::DownloadOutcome::Success { crc, .. } => {
                    tracing::debug!(nzb_id, file_idx, seg_idx, article_size, "download success");
                    if nzb.download_start_time.is_none() {
                        nzb.download_start_time = Some(std::time::SystemTime::now());
                    }
                    if let Some(file) = nzb.files.get_mut(file_idx) {
                        if let Some(seg) = file.articles.get_mut(seg_idx) {
                            seg.status = ArticleStatus::Finished;
                            seg.crc = crc;
                        }
                        file.success_size += article_size;
                        file.remaining_size = file.remaining_size.saturating_sub(article_size);
                        file.success_articles += 1;
                    }
                    nzb.success_size += article_size;
                    nzb.remaining_size = nzb.remaining_size.saturating_sub(article_size);
                    nzb.success_article_count += 1;
                    self.session_downloaded += article_size;
                    if file_is_par {
                        nzb.par_current_success_size += article_size;
                        nzb.par_remaining_size =
                            nzb.par_remaining_size.saturating_sub(article_size);
                    }
                }
                crate::command::DownloadOutcome::Failure { .. } => {
                    tracing::debug!(nzb_id, file_idx, seg_idx, article_size, "download failure");
                    if let Some(file) = nzb.files.get_mut(file_idx) {
                        if let Some(seg) = file.articles.get_mut(seg_idx) {
                            seg.status = ArticleStatus::Failed;
                        }
                        file.failed_size += article_size;
                        file.remaining_size = file.remaining_size.saturating_sub(article_size);
                        file.failed_articles += 1;
                    }
                    nzb.failed_size += article_size;
                    nzb.remaining_size = nzb.remaining_size.saturating_sub(article_size);
                    nzb.failed_article_count += 1;
                    if file_is_par {
                        nzb.par_failed_size += article_size;
                        nzb.par_remaining_size =
                            nzb.par_remaining_size.saturating_sub(article_size);
                        nzb.par_failed_article_count += 1;
                    }
                }
                crate::command::DownloadOutcome::Blocked { ref message } => {
                    tracing::warn!(
                        nzb_id, file_idx, seg_idx,
                        reason = %message,
                        "download blocked, pausing downloads"
                    );
                    if let Some(file) = nzb.files.get_mut(file_idx)
                        && let Some(seg) = file.articles.get_mut(seg_idx)
                    {
                        seg.status = ArticleStatus::Undefined;
                    }
                    self.paused = true;
                    return;
                }
            }
        }
        self.update_health(nzb_id);
        self.check_health_failure(nzb_id);
        self.check_file_completion(nzb_id, file_idx);
        self.check_nzb_completion(nzb_id);
    }

    fn check_file_completion(&mut self, nzb_id: u32, file_idx: usize) {
        if let Some(nzb) = self.queue.queue.iter_mut().find(|n| n.id == nzb_id)
            && let Some(file) = nzb.files.get_mut(file_idx)
            && !file.completed
            && file
                .articles
                .iter()
                .all(|a| a.status == ArticleStatus::Finished || a.status == ArticleStatus::Failed)
        {
            file.completed = true;
            nzb.remaining_file_count = nzb.remaining_file_count.saturating_sub(1);
            if is_par_file(&file.filename) {
                nzb.remaining_par_count = nzb.remaining_par_count.saturating_sub(1);
            }
            tracing::info!("file {} completed for NZB {}", file.filename, nzb.name);
        }
    }

    fn check_nzb_completion(&mut self, nzb_id: u32) {
        let all_complete = self
            .queue
            .queue
            .iter()
            .find(|n| n.id == nzb_id)
            .is_some_and(|nzb| nzb.files.iter().all(|f| f.completed));

        if !all_complete {
            return;
        }

        let has_completion_tx = self.completion_tx.is_some();

        if let Some(nzb) = self.queue.queue.iter_mut().find(|n| n.id == nzb_id) {
            tracing::info!(
                "NZB {} completed download, starting post-processing",
                nzb.name
            );

            if let Some(start) = nzb.download_start_time {
                nzb.download_sec = start.elapsed().map(|d| d.as_secs()).unwrap_or(0);
            }

            let now = std::time::SystemTime::now();
            nzb.post_info = Some(bergamot_core::models::PostInfo {
                nzb_id: nzb.id,
                stage: bergamot_core::models::PostStage::Queued,
                progress_label: String::new(),
                file_progress: 0.0,
                stage_progress: 0.0,
                start_time: now,
                stage_time: now,
                working: true,
                messages: Vec::new(),
            });

            if let Some(tx) = &self.completion_tx {
                let notice = NzbCompletionNotice {
                    nzb_id: nzb.id,
                    nzb_name: nzb.name.clone(),
                    working_dir: nzb.temp_dir.clone(),
                    category: if nzb.category.is_empty() {
                        None
                    } else {
                        Some(nzb.category.clone())
                    },
                    parameters: nzb
                        .parameters
                        .iter()
                        .map(|p| (p.name.clone(), p.value.clone()))
                        .collect(),
                };
                if tx.try_send(notice).is_err() {
                    tracing::warn!("postproc channel full or closed for NZB {}", nzb.name);
                }
            }
        }

        if !has_completion_tx
            && let Some(idx) = self.queue.queue.iter().position(|n| n.id == nzb_id)
        {
            let mut nzb = self.queue.queue.remove(idx);
            if let Some(start) = nzb.download_start_time {
                nzb.download_sec = start.elapsed().map(|d| d.as_secs()).unwrap_or(0);
            }
            tracing::info!(
                "NZB {} completed (no post-processor), moved to history",
                nzb.name
            );
            self.add_to_history(nzb, HistoryKind::Nzb);
        }
    }

    fn update_health(&mut self, nzb_id: u32) {
        if let Some(nzb) = self.queue.queue.iter_mut().find(|n| n.id == nzb_id) {
            let data_total = nzb
                .total_article_count
                .saturating_sub(nzb.par_total_article_count);
            let data_failed = nzb
                .failed_article_count
                .saturating_sub(nzb.par_failed_article_count);
            nzb.health = calculate_health(data_total, data_failed);
            nzb.critical_health =
                calculate_critical_health(data_total, nzb.par_size, nzb.par_failed_size);
        }
    }

    fn check_health_failure(&mut self, nzb_id: u32) {
        let should_fail = self
            .queue
            .queue
            .iter()
            .find(|n| n.id == nzb_id)
            .is_some_and(|nzb| nzb.health < nzb.critical_health);

        if should_fail && let Some(idx) = self.queue.queue.iter().position(|n| n.id == nzb_id) {
            let mut nzb = self.queue.queue.remove(idx);
            let has_par = nzb.par_size > 0;
            let health_pct = nzb.health as f64 / 10.0;
            let critical_pct = nzb.critical_health as f64 / 10.0;
            if has_par {
                tracing::warn!(
                    "NZB {} health {:.1}% below critical {:.1}%, too many failed articles to repair with par2, marking as failed",
                    nzb.name,
                    health_pct,
                    critical_pct
                );
            } else {
                tracing::warn!(
                    "NZB {} health {:.1}% — no par2 files available for repair, marking as failed ({} of {} articles failed)",
                    nzb.name,
                    health_pct,
                    nzb.failed_article_count,
                    nzb.total_article_count
                );
            }
            nzb.delete_status = bergamot_core::models::DeleteStatus::Health;
            self.add_to_history(nzb, HistoryKind::Nzb);
        }
    }

    fn check_duplicate(
        &self,
        dup_key: &str,
        dup_mode: bergamot_core::models::DupMode,
    ) -> Option<u32> {
        if dup_key.is_empty() {
            return None;
        }
        for nzb in &self.queue.queue {
            if nzb.dup_key == dup_key {
                return Some(nzb.id);
            }
        }
        for hist in &self.queue.history {
            if hist.nzb_info.dup_key == dup_key {
                match dup_mode {
                    bergamot_core::models::DupMode::All => return Some(hist.id),
                    bergamot_core::models::DupMode::Force => {}
                    bergamot_core::models::DupMode::Score => return Some(hist.id),
                }
            }
        }
        None
    }

    fn unpause_par_files(&mut self, nzb_id: u32) {
        if let Some(nzb) = self.queue.queue.iter_mut().find(|n| n.id == nzb_id) {
            for file in &mut nzb.files {
                if file.paused && is_par_file(&file.filename) {
                    file.paused = false;
                    tracing::info!("unpaused par file {} for NZB {}", file.filename, nzb.name);
                }
            }
        }
    }

    fn apply_edit(&mut self, action: EditAction, ids: Vec<u32>) -> Result<(), QueueError> {
        match action {
            EditAction::Move(position) => {
                for id in ids {
                    self.move_nzb(id, position)?;
                }
                Ok(())
            }
            EditAction::Pause => {
                for id in ids {
                    if let Some(nzb) = self.queue.queue.iter_mut().find(|nzb| nzb.id == id) {
                        nzb.paused = true;
                    }
                }
                Ok(())
            }
            EditAction::Resume => {
                for id in ids {
                    if let Some(nzb) = self.queue.queue.iter_mut().find(|nzb| nzb.id == id) {
                        nzb.paused = false;
                    }
                }
                Ok(())
            }
            EditAction::Delete { .. } => {
                for id in ids {
                    self.queue.queue.retain(|nzb| nzb.id != id);
                }
                Ok(())
            }
            EditAction::SetPriority(priority) => {
                for id in ids {
                    if let Some(nzb) = self.queue.queue.iter_mut().find(|nzb| nzb.id == id) {
                        nzb.priority = priority;
                    }
                }
                Ok(())
            }
            EditAction::SetCategory(category) => {
                for id in ids {
                    if let Some(nzb) = self.queue.queue.iter_mut().find(|nzb| nzb.id == id) {
                        nzb.category = category.clone();
                    }
                }
                Ok(())
            }
            EditAction::SetParameter { .. }
            | EditAction::Merge { .. }
            | EditAction::Split { .. }
            | EditAction::SetName { .. }
            | EditAction::SetDupeKey { .. }
            | EditAction::SetDupeScore(_)
            | EditAction::SetDupeMode(_) => Ok(()),
        }
    }

    fn move_nzb(&mut self, id: u32, position: MovePosition) -> Result<(), QueueError> {
        let index = self
            .queue
            .queue
            .iter()
            .position(|nzb| nzb.id == id)
            .ok_or(QueueError::NzbNotFound(id))?;
        let nzb = self.queue.queue.remove(index);
        let new_index = match position {
            MovePosition::Top => 0,
            MovePosition::Bottom => self.queue.queue.len(),
            MovePosition::Up(steps) => index.saturating_sub(steps as usize),
            MovePosition::Down(steps) => (index + steps as usize).min(self.queue.queue.len()),
            MovePosition::Before(other) => self
                .queue
                .queue
                .iter()
                .position(|entry| entry.id == other)
                .ok_or(QueueError::InvalidMove)?,
            MovePosition::After(other) => self
                .queue
                .queue
                .iter()
                .position(|entry| entry.id == other)
                .map(|pos| pos + 1)
                .ok_or(QueueError::InvalidMove)?,
        };
        let insert_index = new_index.min(self.queue.queue.len());
        self.queue.queue.insert(insert_index, nzb);
        Ok(())
    }

    fn build_snapshot(&self) -> QueueSnapshot {
        QueueSnapshot {
            nzbs: self
                .queue
                .queue
                .iter()
                .map(|nzb| {
                    let final_dir = if nzb.final_dir.as_os_str().is_empty() {
                        None
                    } else {
                        Some(nzb.final_dir.clone())
                    };
                    let active_downloads = self
                        .active_downloads
                        .keys()
                        .filter(|id| id.nzb_id == nzb.id)
                        .count() as u32;
                    NzbSnapshotEntry {
                        id: nzb.id,
                        name: nzb.name.clone(),
                        filename: nzb.filename.clone(),
                        url: nzb.url.clone(),
                        category: nzb.category.clone(),
                        dest_dir: nzb.dest_dir.clone(),
                        final_dir,
                        priority: nzb.priority,
                        paused: nzb.paused,
                        dupe_key: nzb.dup_key.clone(),
                        dupe_score: nzb.dup_score,
                        dupe_mode: nzb.dup_mode,
                        added_time: nzb.added_time,
                        total_size: nzb.size,
                        downloaded_size: nzb.success_size,
                        failed_size: nzb.failed_size,
                        health: nzb.health,
                        critical_health: nzb.critical_health,
                        total_article_count: nzb.total_article_count,
                        success_article_count: nzb.success_article_count,
                        failed_article_count: nzb.failed_article_count,
                        parameters: nzb
                            .parameters
                            .iter()
                            .map(|p| (p.name.clone(), p.value.clone()))
                            .collect(),
                        active_downloads,
                        file_count: nzb.file_count,
                        remaining_file_count: nzb.remaining_file_count,
                        remaining_par_count: nzb.remaining_par_count,
                        file_ids: nzb.files.iter().map(|f| f.id).collect(),
                        post_stage: nzb.post_info.as_ref().map(|pi| pi.stage),
                        post_stage_progress: nzb
                            .post_info
                            .as_ref()
                            .map(|pi| (pi.stage_progress * 1000.0) as u32)
                            .unwrap_or(0),
                        post_info_text: nzb
                            .post_info
                            .as_ref()
                            .map(|pi| pi.progress_label.clone())
                            .unwrap_or_default(),
                        post_stage_time_sec: nzb
                            .post_info
                            .as_ref()
                            .map(|pi| pi.stage_time.elapsed().map(|d| d.as_secs()).unwrap_or(0))
                            .unwrap_or(0),
                        post_total_time_sec: nzb
                            .post_info
                            .as_ref()
                            .map(|pi| pi.start_time.elapsed().map(|d| d.as_secs()).unwrap_or(0))
                            .unwrap_or(0),
                    }
                })
                .collect(),
            history: self.build_history_list(),
            next_nzb_id: self.queue.next_nzb_id,
            next_file_id: self.queue.next_file_id,
            download_paused: self.paused,
            speed_limit: self.download_rate,
        }
    }

    fn build_scheduler_stats(&self) -> Vec<crate::status::SchedulerSlotStats> {
        let Some(ref sched) = self.server_scheduler else {
            return Vec::new();
        };
        sched
            .slots()
            .iter()
            .map(|slot| crate::status::SchedulerSlotStats {
                server_id: slot.server_id,
                server_name: slot.server_name.clone(),
                level: slot.level,
                max_connections: slot.max_connections,
                active_count: slot.active_count,
                pending_bytes: slot.pending_bytes,
                ewma_bytes_per_sec: slot.weight(),
                wfq_ratio: slot.wfq_ratio(),
                in_backoff: slot.in_backoff,
                total_bytes_downloaded: slot
                    .total_bytes_downloaded
                    .load(std::sync::atomic::Ordering::Relaxed),
                total_articles_success: slot
                    .total_articles_success
                    .load(std::sync::atomic::Ordering::Relaxed),
                total_articles_failed: slot
                    .total_articles_failed
                    .load(std::sync::atomic::Ordering::Relaxed),
            })
            .collect()
    }

    fn file_mut(&mut self, nzb_id: u32, file_index: u32) -> Option<&mut FileInfo> {
        self.queue
            .queue
            .iter_mut()
            .find(|nzb| nzb.id == nzb_id)
            .and_then(|nzb| nzb.files.get_mut(file_index as usize))
    }

    fn build_file_list(
        &self,
        nzb_id: u32,
    ) -> Result<Vec<crate::status::FileListEntry>, QueueError> {
        let nzb = self
            .queue
            .queue
            .iter()
            .find(|n| n.id == nzb_id)
            .ok_or(QueueError::NzbNotFound(nzb_id))?;
        Ok(nzb
            .files
            .iter()
            .map(|f| crate::status::FileListEntry {
                id: f.id,
                nzb_id: f.nzb_id,
                filename: f.filename.clone(),
                subject: f.subject.clone(),
                size: f.size,
                remaining_size: f.remaining_size,
                paused: f.paused,
                total_articles: f.total_articles,
                success_articles: f.success_articles,
                failed_articles: f.failed_articles,
                active_downloads: f.active_downloads,
                completed: f.completed,
            })
            .collect())
    }

    fn build_all_file_article_states(&self) -> Vec<FileArticleSnapshot> {
        let mut states = Vec::new();
        for nzb in &self.queue.queue {
            for file in &nzb.files {
                let completed_articles: Vec<(u32, u32)> = file
                    .articles
                    .iter()
                    .enumerate()
                    .filter(|(_, a)| a.status == ArticleStatus::Finished)
                    .map(|(idx, a)| (idx as u32, a.crc))
                    .collect();
                if !completed_articles.is_empty() {
                    states.push(FileArticleSnapshot {
                        file_id: file.id,
                        total_articles: file.total_articles,
                        completed_articles,
                    });
                }
            }
        }
        states
    }

    fn build_history_list(&self) -> Vec<HistoryListEntry> {
        self.queue
            .history
            .iter()
            .map(|h| HistoryListEntry {
                id: h.id,
                name: h.nzb_info.name.clone(),
                category: h.nzb_info.category.clone(),
                kind: h.kind,
                time: h.time,
                size: h.nzb_info.size,
                par_status: h.nzb_info.par_status,
                unpack_status: h.nzb_info.unpack_status,
                move_status: h.nzb_info.move_status,
                delete_status: h.nzb_info.delete_status,
                mark_status: h.nzb_info.mark_status,
                script_status: h.nzb_info.script_status,
                health: h.nzb_info.health,
                file_count: h.nzb_info.file_count,
                remaining_par_count: h.nzb_info.remaining_par_count,
                total_article_count: h.nzb_info.total_article_count,
                success_article_count: h.nzb_info.success_article_count,
                failed_article_count: h.nzb_info.failed_article_count,
                download_time_sec: h.nzb_info.download_sec,
                post_total_sec: h.nzb_info.post_total_sec,
                par_sec: h.nzb_info.par_sec,
                repair_sec: h.nzb_info.repair_sec,
                unpack_sec: h.nzb_info.unpack_sec,
            })
            .collect()
    }

    fn history_return_to_queue(&mut self, history_id: u32) -> Result<u32, QueueError> {
        let idx = self
            .queue
            .history
            .iter()
            .position(|h| h.id == history_id)
            .ok_or(QueueError::NzbNotFound(history_id))?;
        let entry = self.queue.history.remove(idx);
        let new_id = self.queue.next_nzb_id;
        self.queue.next_nzb_id += 1;
        let mut nzb = entry.nzb_info;
        nzb.id = new_id;
        nzb.deleted = false;
        nzb.paused = false;
        self.queue.queue.push(nzb);
        Ok(new_id)
    }

    fn history_redownload(&mut self, history_id: u32) -> Result<u32, QueueError> {
        let idx = self
            .queue
            .history
            .iter()
            .position(|h| h.id == history_id)
            .ok_or(QueueError::NzbNotFound(history_id))?;
        let entry = self.queue.history.remove(idx);
        let new_id = self.queue.next_nzb_id;
        self.queue.next_nzb_id += 1;
        let mut nzb = entry.nzb_info;
        nzb.id = new_id;
        nzb.deleted = false;
        nzb.paused = false;
        nzb.success_article_count = 0;
        nzb.failed_article_count = 0;
        nzb.success_size = 0;
        nzb.failed_size = 0;
        nzb.remaining_size = nzb.size;
        nzb.current_downloaded_size = 0;
        nzb.par_status = bergamot_core::models::ParStatus::None;
        nzb.unpack_status = bergamot_core::models::UnpackStatus::None;
        nzb.move_status = bergamot_core::models::MoveStatus::None;
        nzb.delete_status = bergamot_core::models::DeleteStatus::None;
        nzb.mark_status = bergamot_core::models::MarkStatus::None;
        nzb.health = 1000;
        for file in &mut nzb.files {
            file.completed = false;
            file.paused = false;
            file.success_articles = 0;
            file.failed_articles = 0;
            file.missing_articles = 0;
            file.success_size = 0;
            file.failed_size = 0;
            file.missed_size = 0;
            file.remaining_size = file.size;
            for article in &mut file.articles {
                article.status = ArticleStatus::Undefined;
            }
        }
        self.queue.queue.push(nzb);
        Ok(new_id)
    }

    fn history_mark(
        &mut self,
        history_id: u32,
        mark: bergamot_core::models::MarkStatus,
    ) -> Result<(), QueueError> {
        let entry = self
            .queue
            .history
            .iter_mut()
            .find(|h| h.id == history_id)
            .ok_or(QueueError::NzbNotFound(history_id))?;
        entry.nzb_info.mark_status = mark;
        Ok(())
    }

    fn history_delete(&mut self, history_id: u32) -> Result<(), QueueError> {
        let len_before = self.queue.history.len();
        self.queue.history.retain(|h| h.id != history_id);
        if self.queue.history.len() == len_before {
            Err(QueueError::NzbNotFound(history_id))
        } else {
            Ok(())
        }
    }

    pub fn ingest_nzb(
        &mut self,
        path: &std::path::Path,
        category: Option<String>,
        priority: Priority,
    ) -> Result<u32, QueueError> {
        let data = std::fs::read(path)
            .map_err(|e| QueueError::IoError(format!("{}: {e}", path.display())))?;

        if let Ok(xml_str) = std::str::from_utf8(&data) {
            tracing::debug!(path = %path.display(), "NZB XML content:\n{xml_str}");
        } else {
            tracing::debug!(path = %path.display(), bytes = data.len(), "NZB content is binary (gzipped)");
        }

        let parsed = bergamot_nzb::parse_nzb_auto(&data)
            .map_err(|e| QueueError::NzbParse(format!("{e}")))?;

        for nzb_file in &parsed.files {
            tracing::info!(
                subject = %nzb_file.subject,
                filename = ?nzb_file.filename,
                par_status = ?nzb_file.par_status,
                "NZB file par classification"
            );
        }
        let has_pars = parsed
            .files
            .iter()
            .any(|f| f.par_status != bergamot_nzb::ParStatus::NotPar);
        tracing::info!(
            path = %path.display(),
            file_count = parsed.files.len(),
            has_pars,
            "NZB par detection summary"
        );

        let id = self.queue.next_nzb_id;
        self.queue.next_nzb_id += 1;

        let name = path
            .file_name()
            .map(|v| v.to_string_lossy().to_string())
            .unwrap_or_else(|| "nzb".to_string());
        let dup_key = name.strip_suffix(".nzb").unwrap_or(&name).to_string();

        if let Some(existing_id) =
            self.check_duplicate(&dup_key, bergamot_core::models::DupMode::Score)
        {
            tracing::info!(
                "duplicate NZB detected: {} matches existing id {}",
                name,
                existing_id
            );
        }

        let mut files = Vec::with_capacity(parsed.files.len());
        let mut total_size: u64 = 0;
        let mut total_article_count: u32 = 0;
        let mut par_size: u64 = 0;
        let mut par_file_count: u32 = 0;
        let mut par_article_count: u32 = 0;

        for (file_idx, nzb_file) in parsed.files.iter().enumerate() {
            let file_id = self.queue.next_file_id;
            self.queue.next_file_id += 1;

            let output_filename = nzb_file
                .filename
                .clone()
                .unwrap_or_else(|| format!("file-{file_idx}"));

            let is_par = nzb_file.par_status != bergamot_nzb::ParStatus::NotPar;

            let articles: Vec<bergamot_core::models::ArticleInfo> = nzb_file
                .segments
                .iter()
                .map(|seg| bergamot_core::models::ArticleInfo {
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

            if is_par {
                par_size += file_size;
                par_file_count += 1;
                par_article_count += article_count;
            }

            files.push(FileInfo {
                id: file_id,
                nzb_id: id,
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

        let file_count = files.len() as u32;
        let nzb = NzbInfo {
            id,
            kind: bergamot_core::models::NzbKind::Nzb,
            name,
            filename: path.to_string_lossy().to_string(),
            url: String::new(),
            dest_dir: self.dest_dir.clone(),
            final_dir: std::path::PathBuf::new(),
            temp_dir: self.inter_dir.join(format!("nzb-{id}")),
            queue_dir: std::path::PathBuf::new(),
            category: category.unwrap_or_default(),
            priority,
            dup_key,
            dup_mode: bergamot_core::models::DupMode::Score,
            dup_score: 0,
            size: total_size,
            remaining_size: total_size,
            paused_size: 0,
            failed_size: 0,
            success_size: 0,
            current_downloaded_size: 0,
            par_size,
            par_remaining_size: par_size,
            par_current_success_size: 0,
            par_failed_size: 0,
            par_total_article_count: par_article_count,
            par_failed_article_count: 0,
            file_count,
            remaining_file_count: file_count,
            remaining_par_count: par_file_count,
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
            critical_health: calculate_critical_health(total_article_count, par_size, 0),
            files,
            completed_files: vec![],
            server_stats: vec![],
            parameters: vec![],
            post_info: None,
            message_count: 0,
            cached_message_count: 0,
        };

        self.queue.queue.push(nzb);
        Ok(id)
    }

    pub fn seed_state(&mut self, queue: DownloadQueue, paused: bool, rate: u64) {
        let nzb_count = queue.queue.len();
        self.session_downloaded = queue.queue.iter().map(|nzb| nzb.success_size).sum();
        self.queue = queue;
        self.paused = paused;
        self.download_rate = rate;
        let _ = self.rate_watch_tx.send(rate);
        tracing::info!(nzb_count, paused, rate, "restored queue state from disk");
    }

    fn update_post_status(
        &mut self,
        nzb_id: u32,
        par_status: Option<bergamot_core::models::ParStatus>,
        unpack_status: Option<bergamot_core::models::UnpackStatus>,
        move_status: Option<bergamot_core::models::MoveStatus>,
    ) {
        let nzb = self
            .queue
            .queue
            .iter_mut()
            .find(|n| n.id == nzb_id)
            .map(|n| n as &mut NzbInfo)
            .or_else(|| {
                self.queue
                    .history
                    .iter_mut()
                    .find(|h| h.id == nzb_id)
                    .map(|h| &mut h.nzb_info)
            });

        if let Some(nzb) = nzb {
            if let Some(status) = par_status {
                nzb.par_status = status;
            }
            if let Some(status) = unpack_status {
                nzb.unpack_status = status;
            }
            if let Some(status) = move_status {
                nzb.move_status = status;
            }
        }
    }

    fn update_post_stage(&mut self, nzb_id: u32, stage: bergamot_core::models::PostStage) {
        if let Some(nzb) = self.queue.queue.iter_mut().find(|n| n.id == nzb_id) {
            let now = std::time::SystemTime::now();
            let info = nzb
                .post_info
                .get_or_insert_with(|| bergamot_core::models::PostInfo {
                    nzb_id,
                    stage,
                    progress_label: String::new(),
                    file_progress: 0.0,
                    stage_progress: 0.0,
                    start_time: now,
                    stage_time: now,
                    working: true,
                    messages: Vec::new(),
                });
            info.stage = stage;
            info.stage_time = now;
            info.stage_progress = 0.0;
            info.progress_label = match stage {
                bergamot_core::models::PostStage::Queued => "Queued".to_string(),
                bergamot_core::models::PostStage::ParLoading => "Loading par2 files".to_string(),
                bergamot_core::models::PostStage::ParRenaming => "Renaming".to_string(),
                bergamot_core::models::PostStage::ParVerifying => "Verifying".to_string(),
                bergamot_core::models::PostStage::ParRepairing => "Repairing".to_string(),
                bergamot_core::models::PostStage::Unpacking => "Unpacking".to_string(),
                bergamot_core::models::PostStage::Moving => "Moving".to_string(),
                bergamot_core::models::PostStage::Executing => "Executing script".to_string(),
                bergamot_core::models::PostStage::Finished => "Finished".to_string(),
            };
        }
    }

    fn update_post_progress(&mut self, nzb_id: u32, progress: u32) {
        if let Some(nzb) = self.queue.queue.iter_mut().find(|n| n.id == nzb_id)
            && let Some(info) = &mut nzb.post_info
        {
            info.stage_progress = progress as f32 / 1000.0;
        }
    }

    fn finish_post_processing(
        &mut self,
        nzb_id: u32,
        par_status: bergamot_core::models::ParStatus,
        unpack_status: bergamot_core::models::UnpackStatus,
        move_status: bergamot_core::models::MoveStatus,
        timings: crate::command::PostProcessTimings,
    ) {
        if let Some(idx) = self.queue.queue.iter().position(|n| n.id == nzb_id) {
            let mut nzb = self.queue.queue.remove(idx);
            nzb.par_status = par_status;
            nzb.unpack_status = unpack_status;
            nzb.move_status = move_status;
            nzb.post_total_sec = timings.post_total_sec;
            nzb.par_sec = timings.par_sec;
            nzb.repair_sec = timings.repair_sec;
            nzb.unpack_sec = timings.unpack_sec;
            nzb.post_info = None;
            tracing::info!(nzb = %nzb.name, "post-processing finished, moving to history");
            self.add_to_history(nzb, HistoryKind::Nzb);
        }
    }

    pub fn add_to_history(&mut self, nzb: NzbInfo, kind: HistoryKind) {
        let id = nzb.id;
        let entry = HistoryInfo {
            id,
            kind,
            time: std::time::SystemTime::now(),
            nzb_info: nzb,
        };
        self.queue.history.push(entry);
    }
}

pub fn calculate_health(total_articles: u32, failed_articles: u32) -> u32 {
    if total_articles == 0 {
        return 1000;
    }
    let success = total_articles.saturating_sub(failed_articles);
    (1000u64 * success as u64 / total_articles as u64) as u32
}

pub fn calculate_critical_health(total_articles: u32, par_size: u64, par_failed_size: u64) -> u32 {
    if total_articles == 0 || par_size == 0 {
        return 1000;
    }
    let usable_par = par_size.saturating_sub(par_failed_size);
    let par_ratio = usable_par as f64 / par_size as f64;
    let max_repairable = ((total_articles / 10) as f64 * par_ratio) as u32;
    let min_success = total_articles.saturating_sub(max_repairable);
    (1000u64 * min_success as u64 / total_articles as u64) as u32
}

fn is_par_file(filename: &str) -> bool {
    let lower = filename.to_lowercase();
    lower.ends_with(".par2") || lower.contains(".vol")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{DownloadOutcome, DownloadResult};
    use bergamot_core::models::{ArticleInfo, ArticleStatus};

    fn sample_file() -> FileInfo {
        sample_file_named("file")
    }

    fn sample_file_named(name: &str) -> FileInfo {
        FileInfo {
            id: 1,
            nzb_id: 1,
            filename: name.to_string(),
            subject: "subject".to_string(),
            output_filename: name.to_string(),
            groups: vec![],
            articles: vec![ArticleInfo {
                part_number: 1,
                message_id: "<a@b>".to_string(),
                size: 1,
                status: ArticleStatus::Undefined,
                segment_offset: 0,
                segment_size: 1,
                crc: 0,
            }],
            size: 1,
            remaining_size: 1,
            success_size: 0,
            failed_size: 0,
            missed_size: 0,
            total_articles: 1,
            missing_articles: 0,
            failed_articles: 0,
            success_articles: 0,
            paused: false,
            completed: false,
            priority: Priority::Normal,
            time: std::time::SystemTime::UNIX_EPOCH,
            active_downloads: 0,
            crc: 0,
            server_stats: vec![],
        }
    }

    fn sample_nzb(id: u32, name: &str) -> NzbInfo {
        NzbInfo {
            id,
            kind: bergamot_core::models::NzbKind::Nzb,
            name: name.to_string(),
            filename: name.to_string(),
            url: String::new(),
            dest_dir: std::path::PathBuf::new(),
            final_dir: std::path::PathBuf::new(),
            temp_dir: std::path::PathBuf::new(),
            queue_dir: std::path::PathBuf::new(),
            category: String::new(),
            priority: Priority::Normal,
            dup_key: String::new(),
            dup_mode: bergamot_core::models::DupMode::Score,
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
            par_total_article_count: 0,
            par_failed_article_count: 0,
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
            par_status: bergamot_core::models::ParStatus::None,
            unpack_status: bergamot_core::models::UnpackStatus::None,
            move_status: bergamot_core::models::MoveStatus::None,
            delete_status: bergamot_core::models::DeleteStatus::None,
            mark_status: bergamot_core::models::MarkStatus::None,
            url_status: bergamot_core::models::UrlStatus::None,
            script_status: bergamot_core::models::ScriptStatus::None,
            health: 1000,
            critical_health: 1000,
            files: vec![sample_file()],
            completed_files: vec![],
            server_stats: vec![],
            parameters: vec![],
            post_info: None,
            message_count: 0,
            cached_message_count: 0,
        }
    }

    #[tokio::test]
    async fn pause_all_sets_paused_flag() {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        tokio::spawn(async move { coordinator.run().await });

        handle.pause_all().await.expect("pause");
        let status = handle.get_status().await.expect("status");
        assert!(status.paused);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn resume_all_clears_paused_flag() {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        tokio::spawn(async move { coordinator.run().await });

        handle.pause_all().await.expect("pause");
        handle.resume_all().await.expect("resume");
        let status = handle.get_status().await.expect("status");
        assert!(!status.paused);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn set_download_rate_updates_rate() {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        tokio::spawn(async move { coordinator.run().await });

        handle.set_download_rate(500_000).await.expect("rate");
        let status = handle.get_status().await.expect("status");
        assert_eq!(status.download_rate, 500_000);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn set_download_rate_updates_watch_receiver() {
        let (mut coordinator, handle, _rx, mut rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        tokio::spawn(async move { coordinator.run().await });

        assert_eq!(*rate_rx.borrow(), 0);

        handle.set_download_rate(1_000_000).await.expect("rate");
        rate_rx.changed().await.expect("changed");
        assert_eq!(*rate_rx.borrow(), 1_000_000);

        handle.set_download_rate(0).await.expect("rate");
        rate_rx.changed().await.expect("changed");
        assert_eq!(*rate_rx.borrow(), 0);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn get_queue_snapshot_returns_state() {
        let _nzb_file = write_sample_nzb();
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        tokio::spawn(async move { coordinator.run().await });

        handle
            .add_nzb(_nzb_file.path().to_path_buf(), None, Priority::Normal)
            .await
            .expect("add");

        let snapshot = handle.get_queue_snapshot().await.expect("snapshot");
        assert_eq!(snapshot.nzbs.len(), 1);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn add_nzb_updates_queue() {
        let _nzb_file = write_sample_nzb();
        let (mut coordinator, handle, _assignments_rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );

        tokio::spawn(async move {
            coordinator.run().await;
        });

        let id = handle
            .add_nzb(_nzb_file.path().to_path_buf(), None, Priority::Normal)
            .await
            .expect("add nzb");
        assert_eq!(id, 1);

        let status = handle.get_status().await.expect("status");
        assert_eq!(status.queued, 1);

        handle.shutdown().await.expect("shutdown");
    }

    #[test]
    fn move_nzb_reorders_queue() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );

        let nzb = NzbInfo {
            id: 1,
            kind: bergamot_core::models::NzbKind::Nzb,
            name: "first".to_string(),
            filename: "first".to_string(),
            url: String::new(),
            dest_dir: std::path::PathBuf::new(),
            final_dir: std::path::PathBuf::new(),
            temp_dir: std::path::PathBuf::new(),
            queue_dir: std::path::PathBuf::new(),
            category: String::new(),
            priority: Priority::Normal,
            dup_key: String::new(),
            dup_mode: bergamot_core::models::DupMode::Score,
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
            par_total_article_count: 0,
            par_failed_article_count: 0,
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
            par_status: bergamot_core::models::ParStatus::None,
            unpack_status: bergamot_core::models::UnpackStatus::None,
            move_status: bergamot_core::models::MoveStatus::None,
            delete_status: bergamot_core::models::DeleteStatus::None,
            mark_status: bergamot_core::models::MarkStatus::None,
            url_status: bergamot_core::models::UrlStatus::None,
            script_status: bergamot_core::models::ScriptStatus::None,
            health: 1000,
            critical_health: 1000,
            files: vec![sample_file()],
            completed_files: vec![],
            server_stats: vec![],
            parameters: vec![],
            post_info: None,
            message_count: 0,
            cached_message_count: 0,
        };
        let mut second = nzb.clone();
        second.id = 2;
        second.name = "second".to_string();

        coordinator.queue.queue.push(nzb);
        coordinator.queue.queue.push(second);

        coordinator.move_nzb(2, MovePosition::Top).expect("move");
        assert_eq!(coordinator.queue.queue[0].id, 2);
    }

    #[test]
    fn next_article_respects_pause_and_priority() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        let nzb = NzbInfo {
            id: 1,
            kind: bergamot_core::models::NzbKind::Nzb,
            name: "test".to_string(),
            filename: "test".to_string(),
            url: String::new(),
            dest_dir: std::path::PathBuf::new(),
            final_dir: std::path::PathBuf::new(),
            temp_dir: std::path::PathBuf::new(),
            queue_dir: std::path::PathBuf::new(),
            category: String::new(),
            priority: Priority::Low,
            dup_key: String::new(),
            dup_mode: bergamot_core::models::DupMode::Score,
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
            par_total_article_count: 0,
            par_failed_article_count: 0,
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
            paused: true,
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
            files: vec![sample_file()],
            completed_files: vec![],
            server_stats: vec![],
            parameters: vec![],
            post_info: None,
            message_count: 0,
            cached_message_count: 0,
        };
        let mut second = nzb.clone();
        second.id = 2;
        second.priority = Priority::High;
        second.paused = false;

        coordinator.queue.queue.push(nzb);
        coordinator.queue.queue.push(second);

        let next = coordinator.next_article();
        assert_eq!(next.unwrap().nzb_id, 2);
    }

    #[test]
    fn fill_download_slots_returns_assignments() {
        let (mut coordinator, _handle, mut _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        let mut nzb = NzbInfo {
            id: 1,
            kind: bergamot_core::models::NzbKind::Nzb,
            name: "test".to_string(),
            filename: "test".to_string(),
            url: String::new(),
            dest_dir: std::path::PathBuf::new(),
            final_dir: std::path::PathBuf::new(),
            temp_dir: std::path::PathBuf::new(),
            queue_dir: std::path::PathBuf::new(),
            category: String::new(),
            priority: Priority::Normal,
            dup_key: String::new(),
            dup_mode: bergamot_core::models::DupMode::Score,
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
            par_total_article_count: 0,
            par_failed_article_count: 0,
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
            par_status: bergamot_core::models::ParStatus::None,
            unpack_status: bergamot_core::models::UnpackStatus::None,
            move_status: bergamot_core::models::MoveStatus::None,
            delete_status: bergamot_core::models::DeleteStatus::None,
            mark_status: bergamot_core::models::MarkStatus::None,
            url_status: bergamot_core::models::UrlStatus::None,
            script_status: bergamot_core::models::ScriptStatus::None,
            health: 1000,
            critical_health: 1000,
            files: vec![sample_file()],
            completed_files: vec![],
            server_stats: vec![],
            parameters: vec![],
            post_info: None,
            message_count: 0,
            cached_message_count: 0,
        };
        nzb.files[0].articles[0].message_id = "abc@example".to_string();
        coordinator.queue.queue.push(nzb);

        coordinator.try_fill_download_slots();
        let mut assignments = Vec::new();
        while let Ok(a) = _rx.try_recv() {
            assignments.push(a);
        }
        assert_eq!(assignments.len(), 1);
        assert_eq!(assignments[0].message_id, "abc@example");
        assert_eq!(assignments[0].article_id.nzb_id, 1);
    }

    #[test]
    fn download_complete_updates_status() {
        let (mut coordinator, _handle, mut _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        let mut nzb = NzbInfo {
            id: 1,
            kind: bergamot_core::models::NzbKind::Nzb,
            name: "test".to_string(),
            filename: "test".to_string(),
            url: String::new(),
            dest_dir: std::path::PathBuf::new(),
            final_dir: std::path::PathBuf::new(),
            temp_dir: std::path::PathBuf::new(),
            queue_dir: std::path::PathBuf::new(),
            category: String::new(),
            priority: Priority::Normal,
            dup_key: String::new(),
            dup_mode: bergamot_core::models::DupMode::Score,
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
            par_total_article_count: 0,
            par_failed_article_count: 0,
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
            par_status: bergamot_core::models::ParStatus::None,
            unpack_status: bergamot_core::models::UnpackStatus::None,
            move_status: bergamot_core::models::MoveStatus::None,
            delete_status: bergamot_core::models::DeleteStatus::None,
            mark_status: bergamot_core::models::MarkStatus::None,
            url_status: bergamot_core::models::UrlStatus::None,
            script_status: bergamot_core::models::ScriptStatus::None,
            health: 1000,
            critical_health: 1000,
            files: vec![sample_file()],
            completed_files: vec![],
            server_stats: vec![],
            parameters: vec![],
            post_info: None,
            message_count: 0,
            cached_message_count: 0,
        };
        nzb.files[0].articles[0].message_id = "abc@example".to_string();
        coordinator.queue.queue.push(nzb);

        coordinator.try_fill_download_slots();
        let mut assignments = Vec::new();
        while let Ok(a) = _rx.try_recv() {
            assignments.push(a);
        }
        assert_eq!(assignments.len(), 1);
        let article_id = assignments[0].article_id;

        coordinator.handle_download_complete(crate::command::DownloadResult {
            article_id,
            outcome: crate::command::DownloadOutcome::Success {
                data: vec![1, 2, 3],
                offset: 0,
                crc: 0xDEADBEEF,
            },
            server_id: None,
            elapsed: None,
        });

        assert!(coordinator.active_downloads.is_empty());
        assert!(coordinator.queue.queue.is_empty());
        assert_eq!(coordinator.queue.history.len(), 1);
        let nzb = &coordinator.queue.history[0].nzb_info;
        let seg = &nzb.files[0].articles[0];
        assert_eq!(seg.status, ArticleStatus::Finished);
        assert_eq!(seg.crc, 0xDEADBEEF);
        assert_eq!(nzb.success_article_count, 1);
    }

    #[tokio::test]
    async fn get_nzb_list_returns_entries() {
        let _nzb_file1 = write_sample_nzb();
        let _nzb_file2 = write_sample_nzb();
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        tokio::spawn(async move { coordinator.run().await });

        handle
            .add_nzb(_nzb_file1.path().to_path_buf(), None, Priority::Normal)
            .await
            .expect("add");
        handle
            .add_nzb(_nzb_file2.path().to_path_buf(), None, Priority::High)
            .await
            .expect("add");

        let list = handle.get_nzb_list().await.expect("list");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].priority, Priority::Normal);
        assert_eq!(list[1].priority, Priority::High);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn edit_queue_pauses_nzb() {
        let _nzb_file = write_sample_nzb();
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        tokio::spawn(async move { coordinator.run().await });

        let id = handle
            .add_nzb(_nzb_file.path().to_path_buf(), None, Priority::Normal)
            .await
            .expect("add");

        handle
            .edit_queue(EditAction::Pause, vec![id])
            .await
            .expect("edit");

        let list = handle.get_nzb_list().await.expect("list");
        assert_eq!(list.len(), 1);

        handle.shutdown().await.expect("shutdown");
    }

    #[test]
    fn calculate_health_full_success() {
        assert_eq!(calculate_health(100, 0), 1000);
    }

    #[test]
    fn calculate_health_with_failures() {
        assert_eq!(calculate_health(100, 10), 900);
        assert_eq!(calculate_health(100, 50), 500);
    }

    #[test]
    fn calculate_health_empty_returns_1000() {
        assert_eq!(calculate_health(0, 0), 1000);
    }

    #[test]
    fn calculate_critical_health_without_par() {
        assert_eq!(calculate_critical_health(100, 0, 0), 1000);
    }

    #[test]
    fn calculate_critical_health_with_no_par_failures() {
        let ch = calculate_critical_health(100, 500, 0);
        assert_eq!(ch, 900);
    }

    #[test]
    fn calculate_critical_health_with_partial_par_failure() {
        let ch = calculate_critical_health(100, 500, 250);
        assert!(
            ch > 900,
            "partial par failure should reduce repair capacity"
        );
        assert!(
            ch < 1000,
            "partial par failure should still allow some repair"
        );
    }

    #[test]
    fn calculate_critical_health_with_all_par_failed() {
        assert_eq!(calculate_critical_health(100, 500, 500), 1000);
    }

    #[test]
    fn is_par_file_detects_par2_files() {
        assert!(is_par_file("file.par2"));
        assert!(is_par_file("file.PAR2"));
        assert!(is_par_file("file.vol00+01.par2"));
        assert!(!is_par_file("file.rar"));
        assert!(!is_par_file("file.nzb"));
    }

    #[test]
    fn check_duplicate_finds_queued_match() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        let mut nzb = sample_nzb(1, "test");
        nzb.dup_key = "my-dupe-key".to_string();
        coordinator.queue.queue.push(nzb);

        assert!(
            coordinator
                .check_duplicate("my-dupe-key", bergamot_core::models::DupMode::Score)
                .is_some()
        );
    }

    #[test]
    fn check_duplicate_returns_none_for_empty_key() {
        let (coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        assert!(
            coordinator
                .check_duplicate("", bergamot_core::models::DupMode::Score)
                .is_none()
        );
    }

    #[test]
    fn check_duplicate_returns_none_for_no_match() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        let mut nzb = sample_nzb(1, "test");
        nzb.dup_key = "other-key".to_string();
        coordinator.queue.queue.push(nzb);

        assert!(
            coordinator
                .check_duplicate("my-dupe-key", bergamot_core::models::DupMode::Score)
                .is_none()
        );
    }

    #[test]
    fn check_duplicate_force_mode_skips_history() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        let mut hist_nzb = sample_nzb(1, "test");
        hist_nzb.dup_key = "my-dupe-key".to_string();
        coordinator
            .queue
            .history
            .push(bergamot_core::models::HistoryInfo {
                id: 1,
                kind: bergamot_core::models::HistoryKind::Nzb,
                time: std::time::SystemTime::UNIX_EPOCH,
                nzb_info: hist_nzb,
            });

        assert!(
            coordinator
                .check_duplicate("my-dupe-key", bergamot_core::models::DupMode::Force)
                .is_none()
        );
    }

    #[test]
    fn unpause_par_files_unpauses_par2() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        let mut nzb = sample_nzb(1, "test");
        let mut main_file = sample_file_named("data.rar");
        main_file.paused = false;
        let mut par_file = sample_file_named("data.par2");
        par_file.paused = true;
        let mut vol_file = sample_file_named("data.vol00+01.par2");
        vol_file.paused = true;
        nzb.files = vec![main_file, par_file, vol_file];
        coordinator.queue.queue.push(nzb);

        coordinator.unpause_par_files(1);

        assert!(!coordinator.queue.queue[0].files[0].paused);
        assert!(!coordinator.queue.queue[0].files[1].paused);
        assert!(!coordinator.queue.queue[0].files[2].paused);
    }

    #[tokio::test]
    async fn par_unpause_via_handle() {
        let _nzb_file = write_sample_nzb();
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        tokio::spawn(async move { coordinator.run().await });

        handle
            .add_nzb(_nzb_file.path().to_path_buf(), None, Priority::Normal)
            .await
            .expect("add");

        handle.par_unpause(1).await.expect("par_unpause");
        handle.shutdown().await.expect("shutdown");
    }

    #[test]
    fn health_updates_on_download_complete() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        let mut nzb = sample_nzb(1, "test");
        nzb.total_article_count = 10;
        nzb.par_size = 100;
        let mut articles = Vec::new();
        for i in 0..10 {
            articles.push(ArticleInfo {
                part_number: i,
                message_id: format!("<{i}@b>"),
                size: 1,
                status: ArticleStatus::Undefined,
                segment_offset: 0,
                segment_size: 1,
                crc: 0,
            });
        }
        nzb.files = vec![FileInfo {
            articles,
            total_articles: 10,
            ..sample_file()
        }];
        coordinator.queue.queue.push(nzb);

        coordinator.handle_download_complete(crate::command::DownloadResult {
            article_id: ArticleId {
                nzb_id: 1,
                file_idx: 0,
                seg_idx: 0,
            },
            outcome: crate::command::DownloadOutcome::Failure {
                message: "test".to_string(),
            },
            server_id: None,
            elapsed: None,
        });

        assert_eq!(coordinator.queue.queue[0].health, 900);
    }

    fn push_to_history(coordinator: &mut QueueCoordinator, nzb: NzbInfo) {
        coordinator.add_to_history(nzb, HistoryKind::Nzb);
    }

    #[tokio::test]
    async fn get_history_returns_history_entries() {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        let mut nzb = sample_nzb(1, "done");
        nzb.mark_status = bergamot_core::models::MarkStatus::Good;
        push_to_history(&mut coordinator, nzb);

        tokio::spawn(async move { coordinator.run().await });

        let history = handle.get_history().await.expect("get_history");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, 1);
        assert_eq!(history[0].name, "done");
        assert_eq!(
            history[0].mark_status,
            bergamot_core::models::MarkStatus::Good
        );

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn history_return_moves_entry_back_to_queue() {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        coordinator.queue.next_nzb_id = 5;
        let nzb = sample_nzb(1, "returned");
        push_to_history(&mut coordinator, nzb);

        tokio::spawn(async move { coordinator.run().await });

        let new_id = handle.history_return(1).await.expect("return");
        assert_eq!(new_id, 5);

        let history = handle.get_history().await.expect("history");
        assert!(history.is_empty());

        let list = handle.get_nzb_list().await.expect("list");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, 5);
        assert_eq!(list[0].name, "returned");

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn history_return_not_found_returns_error() {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        tokio::spawn(async move { coordinator.run().await });

        let result = handle.history_return(999).await;
        assert!(result.is_err());

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn history_redownload_resets_article_state() {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        coordinator.queue.next_nzb_id = 10;
        let mut nzb = sample_nzb(1, "redownload");
        nzb.size = 100;
        nzb.success_size = 80;
        nzb.failed_size = 20;
        nzb.remaining_size = 0;
        nzb.success_article_count = 8;
        nzb.failed_article_count = 2;
        nzb.health = 800;
        nzb.par_status = bergamot_core::models::ParStatus::Success;
        nzb.unpack_status = bergamot_core::models::UnpackStatus::Success;
        if let Some(article) = nzb.files.first_mut().and_then(|f| f.articles.first_mut()) {
            article.status = ArticleStatus::Finished;
        }
        push_to_history(&mut coordinator, nzb);

        tokio::spawn(async move { coordinator.run().await });

        let new_id = handle.history_redownload(1).await.expect("redownload");
        assert_eq!(new_id, 10);

        let history = handle.get_history().await.expect("history");
        assert!(history.is_empty());

        let snapshot = handle.get_queue_snapshot().await.expect("snapshot");
        assert_eq!(snapshot.nzbs.len(), 1);
        let entry = &snapshot.nzbs[0];
        assert_eq!(entry.id, 10);
        assert_eq!(entry.health, 1000);
        assert_eq!(entry.success_article_count, 0);
        assert_eq!(entry.failed_article_count, 0);
        assert_eq!(entry.downloaded_size, 0);
        assert_eq!(entry.failed_size, 0);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn history_mark_updates_mark_status() {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        let nzb = sample_nzb(1, "mark-me");
        push_to_history(&mut coordinator, nzb);

        tokio::spawn(async move { coordinator.run().await });

        handle
            .history_mark(1, bergamot_core::models::MarkStatus::Bad)
            .await
            .expect("mark");

        let history = handle.get_history().await.expect("history");
        assert_eq!(
            history[0].mark_status,
            bergamot_core::models::MarkStatus::Bad
        );

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn history_mark_not_found_returns_error() {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        tokio::spawn(async move { coordinator.run().await });

        let result = handle
            .history_mark(999, bergamot_core::models::MarkStatus::Good)
            .await;
        assert!(result.is_err());

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn history_delete_removes_entry() {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        let nzb = sample_nzb(1, "delete-me");
        push_to_history(&mut coordinator, nzb);

        tokio::spawn(async move { coordinator.run().await });

        handle.history_delete(1).await.expect("delete");

        let history = handle.get_history().await.expect("history");
        assert!(history.is_empty());

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn history_delete_not_found_returns_error() {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        tokio::spawn(async move { coordinator.run().await });

        let result = handle.history_delete(999).await;
        assert!(result.is_err());

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn snapshot_includes_history() {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        let nzb = sample_nzb(1, "in-history");
        push_to_history(&mut coordinator, nzb);

        tokio::spawn(async move { coordinator.run().await });

        let snapshot = handle.get_queue_snapshot().await.expect("snapshot");
        assert_eq!(snapshot.history.len(), 1);
        assert_eq!(snapshot.history[0].name, "in-history");

        handle.shutdown().await.expect("shutdown");
    }

    fn nzb_with_content_and_par(id: u32) -> NzbInfo {
        let content_file = sample_file_named("data.rar");
        let par_file = sample_file_named("data.par2");
        let mut nzb = sample_nzb(id, "mixed");
        nzb.files = vec![content_file, par_file];
        nzb
    }

    #[test]
    fn next_article_sequential_picks_in_order() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        coordinator.strategy = PostStrategy::Sequential;
        let nzb = nzb_with_content_and_par(1);
        coordinator.queue.queue.push(nzb);

        let article = coordinator.next_article().unwrap();
        assert_eq!(article.file_idx, 0);
    }

    #[test]
    fn next_article_rocket_skips_par_files() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        coordinator.strategy = PostStrategy::Rocket;
        let mut nzb = nzb_with_content_and_par(1);
        nzb.files.reverse();
        coordinator.queue.queue.push(nzb);

        let article = coordinator.next_article().unwrap();
        assert_eq!(article.file_idx, 1);
    }

    #[test]
    fn next_article_rocket_falls_back_to_par() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        coordinator.strategy = PostStrategy::Rocket;
        let mut nzb = sample_nzb(1, "par-only");
        nzb.files = vec![sample_file_named("data.par2")];
        coordinator.queue.queue.push(nzb);

        let article = coordinator.next_article().unwrap();
        assert_eq!(article.file_idx, 0);
    }

    #[test]
    fn next_article_aggressive_picks_par_first() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        coordinator.strategy = PostStrategy::Aggressive;
        let nzb = nzb_with_content_and_par(1);
        coordinator.queue.queue.push(nzb);

        let article = coordinator.next_article().unwrap();
        assert_eq!(article.file_idx, 1);
    }

    #[test]
    fn next_article_aggressive_falls_back_to_content() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        coordinator.strategy = PostStrategy::Aggressive;
        let mut nzb = sample_nzb(1, "content-only");
        nzb.files = vec![sample_file_named("data.rar")];
        coordinator.queue.queue.push(nzb);

        let article = coordinator.next_article().unwrap();
        assert_eq!(article.file_idx, 0);
    }

    #[test]
    fn next_article_balanced_prefers_content() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        coordinator.strategy = PostStrategy::Balanced;
        let mut nzb = nzb_with_content_and_par(1);
        nzb.files.reverse();
        coordinator.queue.queue.push(nzb);

        let article = coordinator.next_article().unwrap();
        assert_eq!(article.file_idx, 1);
    }

    #[tokio::test]
    async fn set_strategy_changes_behavior() {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        let mut nzb = nzb_with_content_and_par(1);
        nzb.files.reverse();
        coordinator.queue.queue.push(nzb);

        assert_eq!(coordinator.next_article().unwrap().file_idx, 0);

        tokio::spawn(async move { coordinator.run().await });

        handle
            .set_strategy(PostStrategy::Rocket)
            .await
            .expect("set_strategy");

        handle.shutdown().await.expect("shutdown");
    }

    const SAMPLE_NZB: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
  <head>
    <meta type="title">Test.Download</meta>
    <meta type="category">tv</meta>
  </head>
  <file poster="user@example.com" date="1706140800"
        subject='Test.Download [01/02] - "data.part01.rar" yEnc (1/2)'>
    <groups><group>alt.binaries.test</group></groups>
    <segments>
      <segment bytes="500" number="1">seg1@example.com</segment>
      <segment bytes="300" number="2">seg2@example.com</segment>
    </segments>
  </file>
  <file poster="user@example.com" date="1706140800"
        subject='Test.Download [02/02] - "data.par2" yEnc (1/1)'>
    <groups><group>alt.binaries.test</group></groups>
    <segments>
      <segment bytes="200" number="1">seg3@example.com</segment>
    </segments>
  </file>
</nzb>
"#;

    fn write_sample_nzb() -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut f = tempfile::Builder::new()
            .suffix(".nzb")
            .tempfile()
            .expect("tempfile");
        f.write_all(SAMPLE_NZB.as_bytes()).expect("write");
        f
    }

    #[test]
    fn ingest_nzb_parses_files_and_articles() {
        let nzb_file = write_sample_nzb();
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );

        let id = coordinator
            .ingest_nzb(nzb_file.path(), Some("tv".to_string()), Priority::High)
            .expect("ingest");
        assert_eq!(id, 1);

        let nzb = &coordinator.queue.queue[0];
        assert_eq!(nzb.files.len(), 2);
        assert_eq!(nzb.category, "tv");
        assert_eq!(nzb.priority, Priority::High);

        let file0 = &nzb.files[0];
        assert_eq!(file0.filename, "data.part01.rar");
        assert_eq!(file0.output_filename, "data.part01.rar");
        assert_eq!(file0.groups, vec!["alt.binaries.test"]);
        assert_eq!(file0.articles.len(), 2);
        assert_eq!(file0.articles[0].message_id, "seg1@example.com");
        assert_eq!(file0.articles[0].size, 500);
        assert_eq!(file0.articles[1].message_id, "seg2@example.com");
        assert_eq!(file0.articles[1].size, 300);
        assert_eq!(file0.size, 800);
        assert_eq!(file0.remaining_size, 800);
        assert_eq!(file0.total_articles, 2);

        let file1 = &nzb.files[1];
        assert_eq!(file1.filename, "data.par2");
        assert_eq!(file1.articles.len(), 1);
        assert_eq!(file1.size, 200);
    }

    #[test]
    fn ingest_nzb_populates_nzb_totals() {
        let nzb_file = write_sample_nzb();
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );

        coordinator
            .ingest_nzb(nzb_file.path(), None, Priority::Normal)
            .expect("ingest");

        let nzb = &coordinator.queue.queue[0];
        assert_eq!(nzb.size, 1000);
        assert_eq!(nzb.remaining_size, 1000);
        assert_eq!(nzb.total_article_count, 3);
        assert_eq!(nzb.file_count, 2);
        assert_eq!(nzb.remaining_file_count, 2);
        assert_eq!(nzb.par_size, 200);
        assert_eq!(nzb.remaining_par_count, 1);
        assert_eq!(nzb.critical_health, calculate_critical_health(3, 200, 0));
    }

    #[test]
    fn ingest_nzb_assigns_file_ids() {
        let nzb_file = write_sample_nzb();
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );

        coordinator
            .ingest_nzb(nzb_file.path(), None, Priority::Normal)
            .expect("ingest");

        let nzb = &coordinator.queue.queue[0];
        assert_eq!(nzb.files[0].id, 1);
        assert_eq!(nzb.files[1].id, 2);
        assert_eq!(nzb.files[0].nzb_id, 1);
        assert_eq!(coordinator.queue.next_file_id, 3);
    }

    #[test]
    fn next_assignment_includes_output_filename() {
        let (coordinator, _handle, _rx, _rate_rx) = ingested_coordinator();
        let (_article_id, assignment) = coordinator.next_assignment().unwrap();
        assert_eq!(assignment.output_filename, "data.part01.rar");
    }

    #[test]
    fn ingest_nzb_returns_error_for_missing_file() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        let result = coordinator.ingest_nzb(
            std::path::Path::new("/nonexistent/test.nzb"),
            None,
            Priority::Normal,
        );
        assert!(result.is_err());
    }

    fn ingested_coordinator() -> (
        QueueCoordinator,
        QueueHandle,
        mpsc::Receiver<ArticleAssignment>,
        watch::Receiver<u64>,
    ) {
        let (mut coordinator, handle, rx, rate_rx) = QueueCoordinator::new(
            4,
            2,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        let nzb_file = write_sample_nzb();
        coordinator
            .ingest_nzb(nzb_file.path(), None, Priority::Normal)
            .expect("ingest");
        (coordinator, handle, rx, rate_rx)
    }

    #[test]
    fn download_success_updates_file_and_nzb_sizes() {
        let (mut coordinator, _handle, _rx, _rate_rx) = ingested_coordinator();
        let article_id = ArticleId {
            nzb_id: 1,
            file_idx: 0,
            seg_idx: 0,
        };
        coordinator.active_downloads.insert(
            article_id,
            ActiveDownload {
                started: Instant::now(),
                server_id: None,
                expected_size: 0,
            },
        );

        coordinator.handle_download_complete(DownloadResult {
            article_id,
            outcome: DownloadOutcome::Success {
                data: vec![0; 500],
                offset: 0,
                crc: 0x1234,
            },
            server_id: None,
            elapsed: None,
        });

        let nzb = &coordinator.queue.queue[0];
        let file = &nzb.files[0];
        assert_eq!(file.success_size, 500);
        assert_eq!(file.remaining_size, 300);
        assert_eq!(file.success_articles, 1);
        assert_eq!(nzb.success_size, 500);
        assert_eq!(nzb.remaining_size, 500);
    }

    #[test]
    fn download_failure_updates_file_and_nzb_sizes() {
        let (mut coordinator, _handle, _rx, _rate_rx) = ingested_coordinator();
        let article_id = ArticleId {
            nzb_id: 1,
            file_idx: 0,
            seg_idx: 0,
        };
        coordinator.active_downloads.insert(
            article_id,
            ActiveDownload {
                started: Instant::now(),
                server_id: None,
                expected_size: 0,
            },
        );

        coordinator.handle_download_complete(DownloadResult {
            article_id,
            outcome: DownloadOutcome::Failure {
                message: "test".to_string(),
            },
            server_id: None,
            elapsed: None,
        });

        let nzb = &coordinator.queue.history[0].nzb_info;
        let file = &nzb.files[0];
        assert_eq!(file.failed_size, 500);
        assert_eq!(file.remaining_size, 300);
        assert_eq!(file.failed_articles, 1);
        assert_eq!(nzb.failed_size, 500);
        assert_eq!(nzb.remaining_size, 500);
        assert_eq!(
            nzb.delete_status,
            bergamot_core::models::DeleteStatus::Health
        );
    }

    fn complete_article(
        coordinator: &mut QueueCoordinator,
        nzb_id: u32,
        file_idx: u32,
        seg_idx: u32,
    ) {
        let article_id = ArticleId {
            nzb_id,
            file_idx,
            seg_idx,
        };
        coordinator.active_downloads.insert(
            article_id,
            ActiveDownload {
                started: Instant::now(),
                server_id: None,
                expected_size: 0,
            },
        );
        coordinator.handle_download_complete(DownloadResult {
            article_id,
            outcome: DownloadOutcome::Success {
                data: vec![],
                offset: 0,
                crc: 0,
            },
            server_id: None,
            elapsed: None,
        });
    }

    #[test]
    fn file_marked_completed_when_all_articles_terminal() {
        let (mut coordinator, _handle, _rx, _rate_rx) = ingested_coordinator();

        complete_article(&mut coordinator, 1, 0, 0);
        assert!(!coordinator.queue.queue[0].files[0].completed);

        complete_article(&mut coordinator, 1, 0, 1);
        assert!(coordinator.queue.queue[0].files[0].completed);
    }

    #[test]
    fn nzb_moved_to_history_when_all_files_completed() {
        let (mut coordinator, _handle, _rx, _rate_rx) = ingested_coordinator();

        complete_article(&mut coordinator, 1, 0, 0);
        complete_article(&mut coordinator, 1, 0, 1);
        assert_eq!(coordinator.queue.queue.len(), 1);

        complete_article(&mut coordinator, 1, 1, 0);
        assert!(coordinator.queue.queue.is_empty());
        assert_eq!(coordinator.queue.history.len(), 1);
        assert!(coordinator.queue.history[0].nzb_info.name.ends_with(".nzb"));
    }

    #[test]
    fn remaining_file_count_decrements_on_file_completion() {
        let (mut coordinator, _handle, _rx, _rate_rx) = ingested_coordinator();

        complete_article(&mut coordinator, 1, 0, 0);
        complete_article(&mut coordinator, 1, 0, 1);
        assert_eq!(coordinator.queue.queue[0].remaining_file_count, 1);
    }

    #[test]
    fn update_post_status_updates_queued_nzb() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        let nzb = sample_nzb(1, "test");
        coordinator.queue.queue.push(nzb);

        coordinator.update_post_status(
            1,
            Some(bergamot_core::models::ParStatus::Success),
            Some(bergamot_core::models::UnpackStatus::Success),
            None,
        );

        assert_eq!(
            coordinator.queue.queue[0].par_status,
            bergamot_core::models::ParStatus::Success
        );
        assert_eq!(
            coordinator.queue.queue[0].unpack_status,
            bergamot_core::models::UnpackStatus::Success
        );
        assert_eq!(
            coordinator.queue.queue[0].move_status,
            bergamot_core::models::MoveStatus::None
        );
    }

    #[test]
    fn update_post_status_updates_history_nzb() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        let nzb = sample_nzb(1, "test");
        push_to_history(&mut coordinator, nzb);

        coordinator.update_post_status(
            1,
            None,
            None,
            Some(bergamot_core::models::MoveStatus::Success),
        );

        assert_eq!(
            coordinator.queue.history[0].nzb_info.move_status,
            bergamot_core::models::MoveStatus::Success
        );
    }

    #[tokio::test]
    async fn update_post_status_via_handle() {
        let _nzb_file = write_sample_nzb();
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        tokio::spawn(async move { coordinator.run().await });

        let id = handle
            .add_nzb(_nzb_file.path().to_path_buf(), None, Priority::Normal)
            .await
            .expect("add");

        handle
            .update_post_status(
                id,
                Some(bergamot_core::models::ParStatus::Failure),
                None,
                None,
            )
            .await
            .expect("update");

        handle.shutdown().await.expect("shutdown");
    }

    #[test]
    fn nzb_completion_emits_notice() {
        let (completion_tx, mut completion_rx) = mpsc::channel(4);
        let (mut coordinator, _handle, mut _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        coordinator.completion_tx = Some(completion_tx);

        let mut nzb = sample_nzb(1, "test-nzb");
        nzb.dest_dir = std::path::PathBuf::from("/downloads/test");
        nzb.temp_dir = std::path::PathBuf::from("/tmp/inter/nzb-1");
        nzb.category = "movies".to_string();
        nzb.total_article_count = 1;
        nzb.file_count = 1;
        nzb.remaining_file_count = 1;
        nzb.files = vec![FileInfo {
            articles: vec![ArticleInfo {
                part_number: 1,
                message_id: "abc@example".to_string(),
                size: 100,
                status: ArticleStatus::Undefined,
                segment_offset: 0,
                segment_size: 100,
                crc: 0,
            }],
            total_articles: 1,
            ..sample_file()
        }];
        coordinator.queue.queue.push(nzb);

        coordinator.try_fill_download_slots();
        let mut assignments = Vec::new();
        while let Ok(a) = _rx.try_recv() {
            assignments.push(a);
        }
        assert_eq!(assignments.len(), 1);

        coordinator.handle_download_complete(crate::command::DownloadResult {
            article_id: assignments[0].article_id,
            outcome: crate::command::DownloadOutcome::Success {
                data: vec![1],
                offset: 0,
                crc: 0,
            },
            server_id: None,
            elapsed: None,
        });

        assert_eq!(coordinator.queue.queue.len(), 1);
        assert!(coordinator.queue.queue[0].post_info.is_some());
        assert_eq!(
            coordinator.queue.queue[0].post_info.as_ref().unwrap().stage,
            bergamot_core::models::PostStage::Queued
        );
        assert!(coordinator.queue.history.is_empty());
        let notice = completion_rx.try_recv().expect("should receive notice");
        assert_eq!(notice.nzb_id, 1);
        assert_eq!(notice.nzb_name, "test-nzb");
        assert_eq!(
            notice.working_dir,
            std::path::PathBuf::from("/tmp/inter/nzb-1")
        );
        assert_eq!(notice.category, Some("movies".to_string()));
    }

    #[test]
    fn nzb_completion_without_completion_tx_still_works() {
        let (mut coordinator, _handle, mut _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );

        let mut nzb = sample_nzb(1, "test-nzb");
        nzb.total_article_count = 1;
        nzb.file_count = 1;
        nzb.remaining_file_count = 1;
        nzb.files = vec![FileInfo {
            articles: vec![ArticleInfo {
                part_number: 1,
                message_id: "abc@example".to_string(),
                size: 100,
                status: ArticleStatus::Undefined,
                segment_offset: 0,
                segment_size: 100,
                crc: 0,
            }],
            total_articles: 1,
            ..sample_file()
        }];
        coordinator.queue.queue.push(nzb);

        coordinator.try_fill_download_slots();
        let mut assignments = Vec::new();
        while let Ok(a) = _rx.try_recv() {
            assignments.push(a);
        }
        coordinator.handle_download_complete(crate::command::DownloadResult {
            article_id: assignments[0].article_id,
            outcome: crate::command::DownloadOutcome::Success {
                data: vec![1],
                offset: 0,
                crc: 0,
            },
            server_id: None,
            elapsed: None,
        });

        assert!(coordinator.queue.queue.is_empty());
        assert_eq!(coordinator.queue.history.len(), 1);
    }

    #[test]
    fn build_all_file_article_states_returns_completed_articles() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        let mut nzb = sample_nzb(1, "test");
        nzb.files = vec![FileInfo {
            id: 10,
            articles: vec![
                ArticleInfo {
                    part_number: 1,
                    message_id: "a@b".to_string(),
                    size: 100,
                    status: ArticleStatus::Finished,
                    segment_offset: 0,
                    segment_size: 100,
                    crc: 0xAABB,
                },
                ArticleInfo {
                    part_number: 2,
                    message_id: "c@d".to_string(),
                    size: 200,
                    status: ArticleStatus::Undefined,
                    segment_offset: 0,
                    segment_size: 200,
                    crc: 0,
                },
                ArticleInfo {
                    part_number: 3,
                    message_id: "e@f".to_string(),
                    size: 300,
                    status: ArticleStatus::Finished,
                    segment_offset: 0,
                    segment_size: 300,
                    crc: 0xCCDD,
                },
            ],
            total_articles: 3,
            ..sample_file()
        }];
        coordinator.queue.queue.push(nzb);

        let states = coordinator.build_all_file_article_states();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].file_id, 10);
        assert_eq!(states[0].total_articles, 3);
        assert_eq!(states[0].completed_articles, vec![(0, 0xAABB), (2, 0xCCDD)]);
    }

    #[test]
    fn build_all_file_article_states_skips_files_with_no_completions() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        let mut nzb = sample_nzb(1, "test");
        nzb.files = vec![FileInfo {
            id: 20,
            total_articles: 1,
            ..sample_file()
        }];
        coordinator.queue.queue.push(nzb);

        let states = coordinator.build_all_file_article_states();
        assert!(states.is_empty());
    }

    #[tokio::test]
    async fn get_all_file_article_states_via_handle() {
        let _nzb_file = write_sample_nzb();
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        tokio::spawn(async move { coordinator.run().await });

        handle
            .add_nzb(_nzb_file.path().to_path_buf(), None, Priority::Normal)
            .await
            .expect("add");

        let states = handle.get_all_file_article_states().await.expect("states");
        assert!(states.is_empty());

        handle.shutdown().await.expect("shutdown");
    }

    fn sample_file_with_articles(name: &str, count: usize) -> FileInfo {
        let articles: Vec<ArticleInfo> = (0..count)
            .map(|i| ArticleInfo {
                part_number: i as u32 + 1,
                message_id: format!("<seg{i}@test>"),
                size: 100,
                status: ArticleStatus::Undefined,
                segment_offset: 0,
                segment_size: 100,
                crc: 0,
            })
            .collect();
        FileInfo {
            id: 1,
            nzb_id: 1,
            filename: name.to_string(),
            subject: "subject".to_string(),
            output_filename: name.to_string(),
            groups: vec![],
            articles,
            size: count as u64 * 100,
            remaining_size: count as u64 * 100,
            success_size: 0,
            failed_size: 0,
            missed_size: 0,
            total_articles: count as u32,
            missing_articles: 0,
            failed_articles: 0,
            success_articles: 0,
            paused: false,
            completed: false,
            priority: Priority::Normal,
            time: std::time::SystemTime::UNIX_EPOCH,
            active_downloads: 0,
            crc: 0,
            server_stats: vec![],
        }
    }

    #[test]
    fn cursor_skips_already_dispatched_segments() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(
            10,
            10,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        let mut nzb = sample_nzb(1, "cursor-test");
        nzb.files = vec![sample_file_with_articles("data.rar", 5)];
        coordinator.queue.queue.push(nzb);

        let a1 = coordinator.next_article().unwrap();
        assert_eq!(a1.seg_idx, 0);
        coordinator.mark_segment_status(a1, SegmentStatus::Downloading);
        coordinator.active_downloads.insert(
            a1,
            ActiveDownload {
                started: std::time::Instant::now(),
                server_id: None,
                expected_size: 100,
            },
        );

        let a2 = coordinator.next_article().unwrap();
        assert_eq!(a2.seg_idx, 1);
    }

    #[test]
    fn active_per_file_counter_increments_and_decrements() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(
            10,
            2,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );
        let mut nzb = sample_nzb(1, "counter-test");
        nzb.files = vec![sample_file_with_articles("data.rar", 5)];
        coordinator.queue.queue.push(nzb);

        let key = (1u32, 0u32);
        assert_eq!(coordinator.active_per_file_count(key), 0);

        coordinator.increment_active_per_file(key);
        assert_eq!(coordinator.active_per_file_count(key), 1);

        coordinator.increment_active_per_file(key);
        assert_eq!(coordinator.active_per_file_count(key), 2);

        coordinator.decrement_active_per_file(key);
        assert_eq!(coordinator.active_per_file_count(key), 1);
    }

    #[test]
    fn seed_state_restores_queue() {
        let (mut coordinator, _handle, _rx, rate_rx) = QueueCoordinator::new(
            4,
            2,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );

        let queue = DownloadQueue {
            queue: vec![sample_nzb(5, "restored")],
            history: vec![],
            next_nzb_id: 10,
            next_file_id: 20,
        };
        coordinator.seed_state(queue, true, 12345);

        assert_eq!(coordinator.queue.queue.len(), 1);
        assert_eq!(coordinator.queue.queue[0].id, 5);
        assert_eq!(coordinator.queue.queue[0].name, "restored");
        assert_eq!(coordinator.queue.next_nzb_id, 10);
        assert_eq!(coordinator.queue.next_file_id, 20);
        assert!(coordinator.paused);
        assert_eq!(coordinator.download_rate, 12345);
        assert_eq!(*rate_rx.borrow(), 12345);
    }
}
