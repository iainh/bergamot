use std::path::PathBuf;

use nzbg_core::models::Priority;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentStatus {
    Undefined,
    Downloading,
    Completed,
    Failed,
}

#[derive(Debug, Clone)]
pub struct QueueStatus {
    pub queued: usize,
    pub paused: bool,
    pub download_rate: u64,
    pub remaining_size: u64,
}

#[derive(Debug, Clone)]
pub struct NzbListEntry {
    pub id: u32,
    pub name: String,
    pub priority: Priority,
}

#[derive(Debug, Clone)]
pub struct QueueSnapshot {
    pub nzbs: Vec<NzbSnapshotEntry>,
    pub history: Vec<HistoryListEntry>,
    pub next_nzb_id: u32,
    pub next_file_id: u32,
    pub download_paused: bool,
    pub speed_limit: u64,
}

#[derive(Debug, Clone)]
pub struct NzbSnapshotEntry {
    pub id: u32,
    pub name: String,
    pub filename: String,
    pub url: String,
    pub category: String,
    pub dest_dir: std::path::PathBuf,
    pub final_dir: Option<std::path::PathBuf>,
    pub priority: Priority,
    pub paused: bool,
    pub dupe_key: String,
    pub dupe_score: i32,
    pub dupe_mode: nzbg_core::models::DupMode,
    pub added_time: std::time::SystemTime,
    pub total_size: u64,
    pub downloaded_size: u64,
    pub failed_size: u64,
    pub health: u32,
    pub critical_health: u32,
    pub total_article_count: u32,
    pub success_article_count: u32,
    pub failed_article_count: u32,
    pub parameters: Vec<(String, String)>,
    pub file_ids: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct HistoryListEntry {
    pub id: u32,
    pub name: String,
    pub category: String,
    pub kind: nzbg_core::models::HistoryKind,
    pub time: std::time::SystemTime,
    pub size: u64,
    pub par_status: nzbg_core::models::ParStatus,
    pub unpack_status: nzbg_core::models::UnpackStatus,
    pub move_status: nzbg_core::models::MoveStatus,
    pub delete_status: nzbg_core::models::DeleteStatus,
    pub mark_status: nzbg_core::models::MarkStatus,
    pub health: u32,
}

#[derive(Debug, Clone)]
pub struct FileListEntry {
    pub id: u32,
    pub nzb_id: u32,
    pub filename: String,
    pub subject: String,
    pub size: u64,
    pub remaining_size: u64,
    pub paused: bool,
    pub total_articles: u32,
    pub success_articles: u32,
    pub failed_articles: u32,
    pub active_downloads: u32,
    pub completed: bool,
}

#[derive(Debug, Clone)]
pub struct FileArticleSnapshot {
    pub file_id: u32,
    pub total_articles: u32,
    pub completed_articles: Vec<(u32, u32)>,
}

#[derive(Debug, Clone)]
pub struct NzbCompletionNotice {
    pub nzb_id: u32,
    pub nzb_name: String,
    pub working_dir: PathBuf,
    pub category: Option<String>,
    pub parameters: Vec<(String, String)>,
}
