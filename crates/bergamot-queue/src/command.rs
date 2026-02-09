use std::time::Duration;

use tokio::sync::oneshot;

use bergamot_core::models::{DupMode, Priority};

use crate::coordinator::ArticleId;
use crate::error::QueueError;
use bergamot_core::models::MarkStatus;

use crate::status::{
    FileArticleSnapshot, FileListEntry, HistoryListEntry, NzbListEntry, QueueSnapshot, QueueStatus,
};

#[derive(Debug)]
pub struct DownloadResult {
    pub article_id: ArticleId,
    pub outcome: DownloadOutcome,
    /// Which server handled this article (for scheduler throughput tracking).
    pub server_id: Option<u32>,
    /// How long the download took (for EWMA throughput estimation).
    pub elapsed: Option<Duration>,
}

#[derive(Debug)]
pub enum DownloadOutcome {
    Success {
        data: Vec<u8>,
        offset: u64,
        crc: u32,
    },
    Failure {
        message: String,
    },
    Blocked {
        message: String,
    },
}

#[derive(Debug)]
pub enum QueueCommand {
    AddNzb {
        path: std::path::PathBuf,
        category: Option<String>,
        priority: Priority,
        reply: oneshot::Sender<Result<u32, QueueError>>,
    },
    RemoveNzb {
        id: u32,
        delete_files: bool,
        reply: oneshot::Sender<Result<(), QueueError>>,
    },
    PauseNzb {
        id: u32,
    },
    ResumeNzb {
        id: u32,
    },
    MoveNzb {
        id: u32,
        position: MovePosition,
    },
    PauseFile {
        nzb_id: u32,
        file_index: u32,
    },
    ResumeFile {
        nzb_id: u32,
        file_index: u32,
    },
    PauseAll,
    ResumeAll,
    SetDownloadRate {
        bytes_per_sec: u64,
    },
    EditQueue {
        action: EditAction,
        ids: Vec<u32>,
        reply: oneshot::Sender<Result<(), QueueError>>,
    },
    GetStatus {
        reply: oneshot::Sender<QueueStatus>,
    },
    GetNzbList {
        reply: oneshot::Sender<Vec<NzbListEntry>>,
    },
    GetQueueSnapshot {
        reply: oneshot::Sender<QueueSnapshot>,
    },
    DownloadComplete(DownloadResult),
    ParUnpause {
        nzb_id: u32,
    },
    GetFileList {
        nzb_id: u32,
        reply: oneshot::Sender<Result<Vec<FileListEntry>, QueueError>>,
    },
    GetHistory {
        reply: oneshot::Sender<Vec<HistoryListEntry>>,
    },
    HistoryReturn {
        history_id: u32,
        reply: oneshot::Sender<Result<u32, QueueError>>,
    },
    HistoryRedownload {
        history_id: u32,
        reply: oneshot::Sender<Result<u32, QueueError>>,
    },
    HistoryMark {
        history_id: u32,
        mark: MarkStatus,
        reply: oneshot::Sender<Result<(), QueueError>>,
    },
    HistoryDelete {
        history_id: u32,
        reply: oneshot::Sender<Result<(), QueueError>>,
    },
    UpdatePostStatus {
        nzb_id: u32,
        par_status: Option<bergamot_core::models::ParStatus>,
        unpack_status: Option<bergamot_core::models::UnpackStatus>,
        move_status: Option<bergamot_core::models::MoveStatus>,
    },
    GetAllFileArticleStates {
        reply: oneshot::Sender<Vec<FileArticleSnapshot>>,
    },
    SetStrategy {
        strategy: bergamot_core::models::PostStrategy,
    },
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum EditAction {
    Move(MovePosition),
    Pause,
    Resume,
    Delete { delete_files: bool },
    SetPriority(Priority),
    SetCategory(String),
    SetParameter { key: String, value: String },
    Merge { target_id: u32 },
    Split { file_indices: Vec<u32> },
    SetName(String),
    SetDupeKey(String),
    SetDupeScore(i32),
    SetDupeMode(DupMode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovePosition {
    Top,
    Bottom,
    Up(u32),
    Down(u32),
    Before(u32),
    After(u32),
}
