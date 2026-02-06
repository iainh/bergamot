use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot, watch};

use nzbg_core::models::{
    ArticleStatus, DownloadQueue, FileInfo, HistoryInfo, HistoryKind, NzbInfo, PostStrategy,
    Priority,
};

use crate::command::{EditAction, MovePosition, QueueCommand};
use crate::error::QueueError;
use crate::status::{
    HistoryListEntry, NzbListEntry, NzbSnapshotEntry, QueueSnapshot, QueueStatus, SegmentStatus,
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
}

#[derive(Debug)]
struct ActiveDownload {
    _started: Instant,
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
        mark: nzbg_core::models::MarkStatus,
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

    pub async fn set_strategy(
        &self,
        strategy: nzbg_core::models::PostStrategy,
    ) -> Result<(), QueueError> {
        self.command_tx
            .send(QueueCommand::SetStrategy { strategy })
            .await
            .map_err(|_| QueueError::Shutdown)
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
    max_connections: usize,
    max_articles_per_file: usize,
    download_rate: u64,
    paused: bool,
    shutdown: bool,
    strategy: PostStrategy,
    command_rx: mpsc::Receiver<QueueCommand>,
    assignment_tx: mpsc::Sender<ArticleAssignment>,
    rate_watch_tx: watch::Sender<u64>,
}

impl QueueCoordinator {
    pub fn new(
        max_connections: usize,
        max_articles_per_file: usize,
    ) -> (
        Self,
        QueueHandle,
        mpsc::Receiver<ArticleAssignment>,
        watch::Receiver<u64>,
    ) {
        let (command_tx, command_rx) = mpsc::channel(64);
        let (assignment_tx, assignment_rx) = mpsc::channel(64);
        let (rate_watch_tx, rate_watch_rx) = watch::channel(0u64);
        let handle = QueueHandle { command_tx };
        let coordinator = Self {
            queue: DownloadQueue {
                queue: Vec::new(),
                history: Vec::new(),
                next_nzb_id: 1,
                next_file_id: 1,
            },
            active_downloads: HashMap::new(),
            max_connections,
            max_articles_per_file,
            download_rate: 0,
            paused: false,
            shutdown: false,
            strategy: PostStrategy::default(),
            command_rx,
            assignment_tx,
            rate_watch_tx,
        };
        (coordinator, handle, assignment_rx, rate_watch_rx)
    }

    pub async fn run(&mut self) {
        let mut slot_timer = tokio::time::interval(Duration::from_millis(100));

        loop {
            tokio::select! {
                Some(cmd) = self.command_rx.recv() => {
                    self.handle_command(cmd).await;
                }
                _ = slot_timer.tick() => {
                    for assignment in self.fill_download_slots() {
                        if self.assignment_tx.send(assignment).await.is_err() {
                            break;
                        }
                    }
                }
            }

            if self.shutdown && self.active_downloads.is_empty() {
                break;
            }
        }
    }

    fn fill_download_slots(&mut self) -> Vec<ArticleAssignment> {
        let mut assignments = Vec::new();
        if self.paused {
            return assignments;
        }

        while self.active_downloads.len() < self.max_connections {
            let Some((article_id, assignment)) = self.next_assignment() else {
                break;
            };

            self.mark_segment_status(article_id, SegmentStatus::Downloading);
            self.active_downloads.insert(
                article_id,
                ActiveDownload {
                    _started: Instant::now(),
                },
            );
            assignments.push(assignment);
        }
        assignments
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
        let mut nzbs = self.queue.queue.iter().collect::<Vec<_>>();
        nzbs.sort_by_key(|nzb| nzb.priority);
        for nzb in nzbs.iter().rev() {
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
                if active_count >= self.max_articles_per_file {
                    continue;
                }
                for (seg_idx, segment) in file.articles.iter().enumerate() {
                    if segment.status == nzbg_core::models::ArticleStatus::Undefined {
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
        self.active_downloads
            .keys()
            .filter(|id| id.nzb_id == nzb_id && id.file_idx == file_idx)
            .count()
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
                SegmentStatus::Undefined => nzbg_core::models::ArticleStatus::Undefined,
                SegmentStatus::Downloading => nzbg_core::models::ArticleStatus::Running,
                SegmentStatus::Completed => nzbg_core::models::ArticleStatus::Finished,
                SegmentStatus::Failed => nzbg_core::models::ArticleStatus::Failed,
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
                if let Some(nzb) = self.queue.queue.iter_mut().find(|nzb| nzb.id == id) {
                    nzb.paused = true;
                }
            }
            QueueCommand::ResumeNzb { id } => {
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
                self.paused = true;
            }
            QueueCommand::ResumeAll => {
                self.paused = false;
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
                let status = QueueStatus {
                    queued: self.queue.queue.len(),
                    paused: self.paused,
                    download_rate: self.download_rate,
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
            QueueCommand::SetStrategy { strategy } => {
                self.strategy = strategy;
            }
            QueueCommand::Shutdown => {
                self.shutdown = true;
            }
        }
    }

    fn handle_download_complete(&mut self, result: crate::command::DownloadResult) {
        self.active_downloads.remove(&result.article_id);
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
                return;
            }

            match result.outcome {
                crate::command::DownloadOutcome::Success { crc, .. } => {
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
                }
                crate::command::DownloadOutcome::Failure { .. } => {
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
                }
            }
        }
        self.update_health(nzb_id);
        self.check_file_completion(nzb_id, file_idx);
        self.check_nzb_completion(nzb_id);
    }

    fn check_file_completion(&mut self, nzb_id: u32, file_idx: usize) {
        if let Some(nzb) = self.queue.queue.iter_mut().find(|n| n.id == nzb_id)
            && let Some(file) = nzb.files.get_mut(file_idx)
            && !file.completed
            && file.articles.iter().all(|a| {
                a.status == ArticleStatus::Finished || a.status == ArticleStatus::Failed
            })
        {
            file.completed = true;
            nzb.remaining_file_count = nzb.remaining_file_count.saturating_sub(1);
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

        if all_complete
            && let Some(idx) = self.queue.queue.iter().position(|n| n.id == nzb_id)
        {
            let nzb = self.queue.queue.remove(idx);
            tracing::info!("NZB {} completed, moving to history", nzb.name);
            self.add_to_history(nzb, HistoryKind::Nzb);
        }
    }

    fn update_health(&mut self, nzb_id: u32) {
        if let Some(nzb) = self.queue.queue.iter_mut().find(|n| n.id == nzb_id) {
            nzb.health = calculate_health(nzb.total_article_count, nzb.failed_article_count);
            nzb.critical_health =
                calculate_critical_health(nzb.total_article_count, nzb.par_remaining_size > 0);
        }
    }

    fn check_duplicate(&self, dup_key: &str, dup_mode: nzbg_core::models::DupMode) -> Option<u32> {
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
                    nzbg_core::models::DupMode::All => return Some(hist.id),
                    nzbg_core::models::DupMode::Force => {}
                    nzbg_core::models::DupMode::Score => return Some(hist.id),
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
                .map(|nzb| NzbSnapshotEntry {
                    id: nzb.id,
                    name: nzb.name.clone(),
                    filename: nzb.filename.clone(),
                    category: nzb.category.clone(),
                    dest_dir: nzb.dest_dir.clone(),
                    priority: nzb.priority,
                    paused: nzb.paused,
                    total_size: nzb.size,
                    downloaded_size: nzb.success_size,
                    failed_size: nzb.failed_size,
                    health: nzb.health,
                    critical_health: nzb.critical_health,
                    total_article_count: nzb.total_article_count,
                    success_article_count: nzb.success_article_count,
                    failed_article_count: nzb.failed_article_count,
                    file_ids: nzb.files.iter().map(|f| f.id).collect(),
                })
                .collect(),
            history: self.build_history_list(),
            next_nzb_id: self.queue.next_nzb_id,
            next_file_id: self.queue.next_file_id,
            download_paused: self.paused,
            speed_limit: self.download_rate,
        }
    }

    fn file_mut(&mut self, nzb_id: u32, file_index: u32) -> Option<&mut FileInfo> {
        self.queue
            .queue
            .iter_mut()
            .find(|nzb| nzb.id == nzb_id)
            .and_then(|nzb| nzb.files.get_mut(file_index as usize))
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
                health: h.nzb_info.health,
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
        nzb.par_status = nzbg_core::models::ParStatus::None;
        nzb.unpack_status = nzbg_core::models::UnpackStatus::None;
        nzb.move_status = nzbg_core::models::MoveStatus::None;
        nzb.delete_status = nzbg_core::models::DeleteStatus::None;
        nzb.mark_status = nzbg_core::models::MarkStatus::None;
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
        mark: nzbg_core::models::MarkStatus,
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
        let parsed = nzbg_nzb::parse_nzb_auto(&data)
            .map_err(|e| QueueError::NzbParse(format!("{e}")))?;

        let id = self.queue.next_nzb_id;
        self.queue.next_nzb_id += 1;

        let name = path
            .file_name()
            .map(|v| v.to_string_lossy().to_string())
            .unwrap_or_else(|| "nzb".to_string());
        let dup_key = name.strip_suffix(".nzb").unwrap_or(&name).to_string();

        if let Some(existing_id) =
            self.check_duplicate(&dup_key, nzbg_core::models::DupMode::Score)
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

        for (file_idx, nzb_file) in parsed.files.iter().enumerate() {
            let file_id = self.queue.next_file_id;
            self.queue.next_file_id += 1;

            let output_filename = nzb_file
                .filename
                .clone()
                .unwrap_or_else(|| format!("file-{file_idx}"));

            let is_par = nzb_file.par_status != nzbg_nzb::ParStatus::NotPar;

            let articles: Vec<nzbg_core::models::ArticleInfo> = nzb_file
                .segments
                .iter()
                .map(|seg| nzbg_core::models::ArticleInfo {
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
        let has_par = par_size > 0;

        let nzb = NzbInfo {
            id,
            kind: nzbg_core::models::NzbKind::Nzb,
            name,
            filename: path.to_string_lossy().to_string(),
            url: String::new(),
            dest_dir: std::path::PathBuf::new(),
            final_dir: std::path::PathBuf::new(),
            temp_dir: std::path::PathBuf::new(),
            queue_dir: std::path::PathBuf::new(),
            category: category.unwrap_or_default(),
            priority,
            dup_key,
            dup_mode: nzbg_core::models::DupMode::Score,
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
            par_status: nzbg_core::models::ParStatus::None,
            unpack_status: nzbg_core::models::UnpackStatus::None,
            move_status: nzbg_core::models::MoveStatus::None,
            delete_status: nzbg_core::models::DeleteStatus::None,
            mark_status: nzbg_core::models::MarkStatus::None,
            url_status: nzbg_core::models::UrlStatus::None,
            health: 1000,
            critical_health: calculate_critical_health(total_article_count, has_par),
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

pub fn calculate_critical_health(total_articles: u32, has_par: bool) -> u32 {
    if total_articles == 0 {
        return 1000;
    }
    if !has_par {
        return 1000;
    }
    let max_repairable = total_articles / 10;
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
    use nzbg_core::models::{ArticleInfo, ArticleStatus};

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
            kind: nzbg_core::models::NzbKind::Nzb,
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
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        handle.pause_all().await.expect("pause");
        let status = handle.get_status().await.expect("status");
        assert!(status.paused);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn resume_all_clears_paused_flag() {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        handle.pause_all().await.expect("pause");
        handle.resume_all().await.expect("resume");
        let status = handle.get_status().await.expect("status");
        assert!(!status.paused);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn set_download_rate_updates_rate() {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        handle.set_download_rate(500_000).await.expect("rate");
        let status = handle.get_status().await.expect("status");
        assert_eq!(status.download_rate, 500_000);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn set_download_rate_updates_watch_receiver() {
        let (mut coordinator, handle, _rx, mut rate_rx) = QueueCoordinator::new(2, 1);
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
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        handle
            .add_nzb(
                _nzb_file.path().to_path_buf(),
                None,
                Priority::Normal,
            )
            .await
            .expect("add");

        let snapshot = handle.get_queue_snapshot().await.expect("snapshot");
        assert_eq!(snapshot.nzbs.len(), 1);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn add_nzb_updates_queue() {
        let _nzb_file = write_sample_nzb();
        let (mut coordinator, handle, _assignments_rx, _rate_rx) = QueueCoordinator::new(2, 1);

        tokio::spawn(async move {
            coordinator.run().await;
        });

        let id = handle
            .add_nzb(
                _nzb_file.path().to_path_buf(),
                None,
                Priority::Normal,
            )
            .await
            .expect("add nzb");
        assert_eq!(id, 1);

        let status = handle.get_status().await.expect("status");
        assert_eq!(status.queued, 1);

        handle.shutdown().await.expect("shutdown");
    }

    #[test]
    fn move_nzb_reorders_queue() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);

        let nzb = NzbInfo {
            id: 1,
            kind: nzbg_core::models::NzbKind::Nzb,
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
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        let nzb = NzbInfo {
            id: 1,
            kind: nzbg_core::models::NzbKind::Nzb,
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
            paused: true,
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
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        let mut nzb = NzbInfo {
            id: 1,
            kind: nzbg_core::models::NzbKind::Nzb,
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

        let assignments = coordinator.fill_download_slots();
        assert_eq!(assignments.len(), 1);
        assert_eq!(assignments[0].message_id, "abc@example");
        assert_eq!(assignments[0].article_id.nzb_id, 1);
    }

    #[test]
    fn download_complete_updates_status() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        let mut nzb = NzbInfo {
            id: 1,
            kind: nzbg_core::models::NzbKind::Nzb,
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

        let assignments = coordinator.fill_download_slots();
        assert_eq!(assignments.len(), 1);
        let article_id = assignments[0].article_id;

        coordinator.handle_download_complete(crate::command::DownloadResult {
            article_id,
            outcome: crate::command::DownloadOutcome::Success {
                data: vec![1, 2, 3],
                offset: 0,
                crc: 0xDEADBEEF,
            },
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
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        handle
            .add_nzb(
                _nzb_file1.path().to_path_buf(),
                None,
                Priority::Normal,
            )
            .await
            .expect("add");
        handle
            .add_nzb(
                _nzb_file2.path().to_path_buf(),
                None,
                Priority::High,
            )
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
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let id = handle
            .add_nzb(
                _nzb_file.path().to_path_buf(),
                None,
                Priority::Normal,
            )
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
        assert_eq!(calculate_critical_health(100, false), 1000);
    }

    #[test]
    fn calculate_critical_health_with_par() {
        let ch = calculate_critical_health(100, true);
        assert!(ch < 1000);
        assert_eq!(ch, 900);
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
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        let mut nzb = sample_nzb(1, "test");
        nzb.dup_key = "my-dupe-key".to_string();
        coordinator.queue.queue.push(nzb);

        assert!(
            coordinator
                .check_duplicate("my-dupe-key", nzbg_core::models::DupMode::Score)
                .is_some()
        );
    }

    #[test]
    fn check_duplicate_returns_none_for_empty_key() {
        let (coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        assert!(
            coordinator
                .check_duplicate("", nzbg_core::models::DupMode::Score)
                .is_none()
        );
    }

    #[test]
    fn check_duplicate_returns_none_for_no_match() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        let mut nzb = sample_nzb(1, "test");
        nzb.dup_key = "other-key".to_string();
        coordinator.queue.queue.push(nzb);

        assert!(
            coordinator
                .check_duplicate("my-dupe-key", nzbg_core::models::DupMode::Score)
                .is_none()
        );
    }

    #[test]
    fn check_duplicate_force_mode_skips_history() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        let mut hist_nzb = sample_nzb(1, "test");
        hist_nzb.dup_key = "my-dupe-key".to_string();
        coordinator
            .queue
            .history
            .push(nzbg_core::models::HistoryInfo {
                id: 1,
                kind: nzbg_core::models::HistoryKind::Nzb,
                time: std::time::SystemTime::UNIX_EPOCH,
                nzb_info: hist_nzb,
            });

        assert!(
            coordinator
                .check_duplicate("my-dupe-key", nzbg_core::models::DupMode::Force)
                .is_none()
        );
    }

    #[test]
    fn unpause_par_files_unpauses_par2() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
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
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        handle
            .add_nzb(
                _nzb_file.path().to_path_buf(),
                None,
                Priority::Normal,
            )
            .await
            .expect("add");

        handle.par_unpause(1).await.expect("par_unpause");
        handle.shutdown().await.expect("shutdown");
    }

    #[test]
    fn health_updates_on_download_complete() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        let mut nzb = sample_nzb(1, "test");
        nzb.total_article_count = 10;
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
        });

        assert_eq!(coordinator.queue.queue[0].health, 900);
    }

    fn push_to_history(coordinator: &mut QueueCoordinator, nzb: NzbInfo) {
        coordinator.add_to_history(nzb, HistoryKind::Nzb);
    }

    #[tokio::test]
    async fn get_history_returns_history_entries() {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        let mut nzb = sample_nzb(1, "done");
        nzb.mark_status = nzbg_core::models::MarkStatus::Good;
        push_to_history(&mut coordinator, nzb);

        tokio::spawn(async move { coordinator.run().await });

        let history = handle.get_history().await.expect("get_history");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, 1);
        assert_eq!(history[0].name, "done");
        assert_eq!(history[0].mark_status, nzbg_core::models::MarkStatus::Good);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn history_return_moves_entry_back_to_queue() {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
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
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let result = handle.history_return(999).await;
        assert!(result.is_err());

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn history_redownload_resets_article_state() {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        coordinator.queue.next_nzb_id = 10;
        let mut nzb = sample_nzb(1, "redownload");
        nzb.size = 100;
        nzb.success_size = 80;
        nzb.failed_size = 20;
        nzb.remaining_size = 0;
        nzb.success_article_count = 8;
        nzb.failed_article_count = 2;
        nzb.health = 800;
        nzb.par_status = nzbg_core::models::ParStatus::Success;
        nzb.unpack_status = nzbg_core::models::UnpackStatus::Success;
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
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        let nzb = sample_nzb(1, "mark-me");
        push_to_history(&mut coordinator, nzb);

        tokio::spawn(async move { coordinator.run().await });

        handle
            .history_mark(1, nzbg_core::models::MarkStatus::Bad)
            .await
            .expect("mark");

        let history = handle.get_history().await.expect("history");
        assert_eq!(history[0].mark_status, nzbg_core::models::MarkStatus::Bad);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn history_mark_not_found_returns_error() {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let result = handle
            .history_mark(999, nzbg_core::models::MarkStatus::Good)
            .await;
        assert!(result.is_err());

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn history_delete_removes_entry() {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
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
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let result = handle.history_delete(999).await;
        assert!(result.is_err());

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn snapshot_includes_history() {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
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
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        coordinator.strategy = PostStrategy::Sequential;
        let nzb = nzb_with_content_and_par(1);
        coordinator.queue.queue.push(nzb);

        let article = coordinator.next_article().unwrap();
        assert_eq!(article.file_idx, 0);
    }

    #[test]
    fn next_article_rocket_skips_par_files() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        coordinator.strategy = PostStrategy::Rocket;
        let mut nzb = nzb_with_content_and_par(1);
        nzb.files.reverse();
        coordinator.queue.queue.push(nzb);

        let article = coordinator.next_article().unwrap();
        assert_eq!(article.file_idx, 1);
    }

    #[test]
    fn next_article_rocket_falls_back_to_par() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        coordinator.strategy = PostStrategy::Rocket;
        let mut nzb = sample_nzb(1, "par-only");
        nzb.files = vec![sample_file_named("data.par2")];
        coordinator.queue.queue.push(nzb);

        let article = coordinator.next_article().unwrap();
        assert_eq!(article.file_idx, 0);
    }

    #[test]
    fn next_article_aggressive_picks_par_first() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        coordinator.strategy = PostStrategy::Aggressive;
        let nzb = nzb_with_content_and_par(1);
        coordinator.queue.queue.push(nzb);

        let article = coordinator.next_article().unwrap();
        assert_eq!(article.file_idx, 1);
    }

    #[test]
    fn next_article_aggressive_falls_back_to_content() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        coordinator.strategy = PostStrategy::Aggressive;
        let mut nzb = sample_nzb(1, "content-only");
        nzb.files = vec![sample_file_named("data.rar")];
        coordinator.queue.queue.push(nzb);

        let article = coordinator.next_article().unwrap();
        assert_eq!(article.file_idx, 0);
    }

    #[test]
    fn next_article_balanced_prefers_content() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        coordinator.strategy = PostStrategy::Balanced;
        let mut nzb = nzb_with_content_and_par(1);
        nzb.files.reverse();
        coordinator.queue.queue.push(nzb);

        let article = coordinator.next_article().unwrap();
        assert_eq!(article.file_idx, 1);
    }

    #[tokio::test]
    async fn set_strategy_changes_behavior() {
        let (mut coordinator, handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
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
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);

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
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);

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
        assert_eq!(
            nzb.critical_health,
            calculate_critical_health(3, true)
        );
    }

    #[test]
    fn ingest_nzb_assigns_file_ids() {
        let nzb_file = write_sample_nzb();
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);

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
    fn ingest_nzb_returns_error_for_missing_file() {
        let (mut coordinator, _handle, _rx, _rate_rx) = QueueCoordinator::new(2, 1);
        let result = coordinator.ingest_nzb(
            std::path::Path::new("/nonexistent/test.nzb"),
            None,
            Priority::Normal,
        );
        assert!(result.is_err());
    }

    fn ingested_coordinator() -> (QueueCoordinator, QueueHandle, mpsc::Receiver<ArticleAssignment>, watch::Receiver<u64>) {
        let (mut coordinator, handle, rx, rate_rx) = QueueCoordinator::new(4, 2);
        let nzb_file = write_sample_nzb();
        coordinator
            .ingest_nzb(nzb_file.path(), None, Priority::Normal)
            .expect("ingest");
        (coordinator, handle, rx, rate_rx)
    }

    #[test]
    fn download_success_updates_file_and_nzb_sizes() {
        let (mut coordinator, _handle, _rx, _rate_rx) = ingested_coordinator();
        let article_id = ArticleId { nzb_id: 1, file_idx: 0, seg_idx: 0 };
        coordinator.active_downloads.insert(article_id, ActiveDownload { _started: Instant::now() });

        coordinator.handle_download_complete(DownloadResult {
            article_id,
            outcome: DownloadOutcome::Success { data: vec![0; 500], offset: 0, crc: 0x1234 },
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
        let article_id = ArticleId { nzb_id: 1, file_idx: 0, seg_idx: 0 };
        coordinator.active_downloads.insert(article_id, ActiveDownload { _started: Instant::now() });

        coordinator.handle_download_complete(DownloadResult {
            article_id,
            outcome: DownloadOutcome::Failure { message: "test".to_string() },
        });

        let nzb = &coordinator.queue.queue[0];
        let file = &nzb.files[0];
        assert_eq!(file.failed_size, 500);
        assert_eq!(file.remaining_size, 300);
        assert_eq!(file.failed_articles, 1);
        assert_eq!(nzb.failed_size, 500);
        assert_eq!(nzb.remaining_size, 500);
    }

    fn complete_article(coordinator: &mut QueueCoordinator, nzb_id: u32, file_idx: u32, seg_idx: u32) {
        let article_id = ArticleId { nzb_id, file_idx, seg_idx };
        coordinator.active_downloads.insert(article_id, ActiveDownload { _started: Instant::now() });
        coordinator.handle_download_complete(DownloadResult {
            article_id,
            outcome: DownloadOutcome::Success { data: vec![], offset: 0, crc: 0 },
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
        assert_eq!(coordinator.queue.history[0].nzb_info.name.ends_with(".nzb"), true);
    }

    #[test]
    fn remaining_file_count_decrements_on_file_completion() {
        let (mut coordinator, _handle, _rx, _rate_rx) = ingested_coordinator();

        complete_article(&mut coordinator, 1, 0, 0);
        complete_article(&mut coordinator, 1, 0, 1);
        assert_eq!(coordinator.queue.queue[0].remaining_file_count, 1);
    }
}
