use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot};

use nzbg_core::models::{DownloadQueue, FileInfo, NzbInfo, Priority};

use crate::command::{EditAction, MovePosition, QueueCommand};
use crate::error::QueueError;
use crate::status::{NzbListEntry, NzbSnapshotEntry, QueueSnapshot, QueueStatus, SegmentStatus};

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
    command_rx: mpsc::Receiver<QueueCommand>,
    assignment_tx: mpsc::Sender<ArticleAssignment>,
}

impl QueueCoordinator {
    pub fn new(
        max_connections: usize,
        max_articles_per_file: usize,
    ) -> (Self, QueueHandle, mpsc::Receiver<ArticleAssignment>) {
        let (command_tx, command_rx) = mpsc::channel(64);
        let (assignment_tx, assignment_rx) = mpsc::channel(64);
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
            command_rx,
            assignment_tx,
        };
        (coordinator, handle, assignment_rx)
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
                let id = self.queue.next_nzb_id;
                self.queue.next_nzb_id += 1;
                let name = path
                    .file_name()
                    .map(|value| value.to_string_lossy().to_string())
                    .unwrap_or_else(|| "nzb".to_string());
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
                    files: Vec::new(),
                    completed_files: Vec::new(),
                    server_stats: Vec::new(),
                    parameters: Vec::new(),
                    post_info: None,
                    message_count: 0,
                    cached_message_count: 0,
                };
                self.queue.queue.push(nzb);
                let _ = reply.send(Ok(id));
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
            QueueCommand::Shutdown => {
                self.shutdown = true;
            }
        }
    }

    fn handle_download_complete(&mut self, result: crate::command::DownloadResult) {
        self.active_downloads.remove(&result.article_id);
        match result.outcome {
            crate::command::DownloadOutcome::Success { crc, .. } => {
                self.mark_segment_status(result.article_id, SegmentStatus::Completed);
                if let Some(nzb) = self
                    .queue
                    .queue
                    .iter_mut()
                    .find(|n| n.id == result.article_id.nzb_id)
                {
                    if let Some(seg) = nzb
                        .files
                        .get_mut(result.article_id.file_idx as usize)
                        .and_then(|f| f.articles.get_mut(result.article_id.seg_idx as usize))
                    {
                        seg.crc = crc;
                    }
                    nzb.success_article_count += 1;
                }
            }
            crate::command::DownloadOutcome::Failure { .. } => {
                self.mark_segment_status(result.article_id, SegmentStatus::Failed);
                if let Some(nzb) = self
                    .queue
                    .queue
                    .iter_mut()
                    .find(|n| n.id == result.article_id.nzb_id)
                {
                    nzb.failed_article_count += 1;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use nzbg_core::models::{ArticleInfo, ArticleStatus};

    fn sample_file() -> FileInfo {
        FileInfo {
            id: 1,
            nzb_id: 1,
            filename: "file".to_string(),
            subject: "subject".to_string(),
            output_filename: "file".to_string(),
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

    #[tokio::test]
    async fn pause_all_sets_paused_flag() {
        let (mut coordinator, handle, _rx) = QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        handle.pause_all().await.expect("pause");
        let status = handle.get_status().await.expect("status");
        assert!(status.paused);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn resume_all_clears_paused_flag() {
        let (mut coordinator, handle, _rx) = QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        handle.pause_all().await.expect("pause");
        handle.resume_all().await.expect("resume");
        let status = handle.get_status().await.expect("status");
        assert!(!status.paused);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn set_download_rate_updates_rate() {
        let (mut coordinator, handle, _rx) = QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        handle.set_download_rate(500_000).await.expect("rate");
        let status = handle.get_status().await.expect("status");
        assert_eq!(status.download_rate, 500_000);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn get_queue_snapshot_returns_state() {
        let (mut coordinator, handle, _rx) = QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        handle
            .add_nzb(
                std::path::PathBuf::from("/tmp/test.nzb"),
                None,
                Priority::Normal,
            )
            .await
            .expect("add");

        let snapshot = handle.get_queue_snapshot().await.expect("snapshot");
        assert_eq!(snapshot.nzbs.len(), 1);
        assert_eq!(snapshot.nzbs[0].name, "test.nzb");

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn add_nzb_updates_queue() {
        let (mut coordinator, handle, _assignments_rx) = QueueCoordinator::new(2, 1);

        tokio::spawn(async move {
            coordinator.run().await;
        });

        let id = handle
            .add_nzb(
                std::path::PathBuf::from("/tmp/test.nzb"),
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
        let (mut coordinator, _handle, _rx) = QueueCoordinator::new(2, 1);

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
        let (mut coordinator, _handle, _rx) = QueueCoordinator::new(2, 1);
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
        let (mut coordinator, _handle, _rx) = QueueCoordinator::new(2, 1);
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
        let (mut coordinator, _handle, _rx) = QueueCoordinator::new(2, 1);
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
        let seg = &coordinator.queue.queue[0].files[0].articles[0];
        assert_eq!(seg.status, ArticleStatus::Finished);
        assert_eq!(seg.crc, 0xDEADBEEF);
        assert_eq!(coordinator.queue.queue[0].success_article_count, 1);
    }

    #[tokio::test]
    async fn get_nzb_list_returns_entries() {
        let (mut coordinator, handle, _rx) = QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        handle
            .add_nzb(
                std::path::PathBuf::from("/tmp/first.nzb"),
                None,
                Priority::Normal,
            )
            .await
            .expect("add");
        handle
            .add_nzb(
                std::path::PathBuf::from("/tmp/second.nzb"),
                None,
                Priority::High,
            )
            .await
            .expect("add");

        let list = handle.get_nzb_list().await.expect("list");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "first.nzb");
        assert_eq!(list[1].name, "second.nzb");
        assert_eq!(list[1].priority, Priority::High);

        handle.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn edit_queue_pauses_nzb() {
        let (mut coordinator, handle, _rx) = QueueCoordinator::new(2, 1);
        tokio::spawn(async move { coordinator.run().await });

        let id = handle
            .add_nzb(
                std::path::PathBuf::from("/tmp/test.nzb"),
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
}
