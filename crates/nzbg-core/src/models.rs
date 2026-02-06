use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NzbInfo {
    pub id: u32,
    pub kind: NzbKind,
    pub name: String,
    pub filename: String,
    pub url: String,
    pub dest_dir: PathBuf,
    pub final_dir: PathBuf,
    pub temp_dir: PathBuf,
    pub queue_dir: PathBuf,
    pub category: String,
    pub priority: Priority,
    pub dup_key: String,
    pub dup_mode: DupMode,
    pub dup_score: i32,
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
    pub file_count: u32,
    pub remaining_file_count: u32,
    pub remaining_par_count: u32,
    pub total_article_count: u32,
    pub success_article_count: u32,
    pub failed_article_count: u32,
    pub added_time: SystemTime,
    pub min_time: Option<SystemTime>,
    pub max_time: Option<SystemTime>,
    pub download_start_time: Option<SystemTime>,
    pub download_sec: u64,
    pub post_total_sec: u64,
    pub par_sec: u64,
    pub repair_sec: u64,
    pub unpack_sec: u64,
    pub paused: bool,
    pub deleted: bool,
    pub direct_rename: bool,
    pub force_priority: bool,
    pub reprocess: bool,
    pub par_manual: bool,
    pub clean_up_disk: bool,
    pub par_status: ParStatus,
    pub unpack_status: UnpackStatus,
    pub move_status: MoveStatus,
    pub delete_status: DeleteStatus,
    pub mark_status: MarkStatus,
    pub url_status: UrlStatus,
    pub health: u32,
    pub critical_health: u32,
    pub files: Vec<FileInfo>,
    pub completed_files: Vec<CompletedFile>,
    pub server_stats: Vec<ServerStat>,
    pub parameters: Vec<NzbParameter>,
    pub post_info: Option<PostInfo>,
    pub message_count: u32,
    pub cached_message_count: u32,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
pub enum NzbKind {
    Nzb,
    Url,
}

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize_repr, Deserialize_repr)]
pub enum Priority {
    VeryLow = -100,
    Low = -50,
    Normal = 0,
    High = 50,
    VeryHigh = 100,
    Force = 900,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
pub enum ParStatus {
    None = 0,
    Failure = 1,
    Success = 2,
    RepairPossible = 3,
    Manual = 4,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
pub enum UnpackStatus {
    None = 0,
    Failure = 1,
    Success = 2,
    Password = 3,
    Space = 4,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
pub enum MoveStatus {
    None = 0,
    Failure = 1,
    Success = 2,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
pub enum DeleteStatus {
    None = 0,
    Manual = 1,
    Health = 2,
    Dupe = 3,
    Bad = 4,
    Scan = 5,
    Copy = 6,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
pub enum MarkStatus {
    None = 0,
    Good = 1,
    Bad = 2,
    Success = 3,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
pub enum UrlStatus {
    None = 0,
    Failure = 1,
    Success = 2,
    ScanFailure = 3,
    ScanSkipped = 4,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
pub enum DupMode {
    Score = 0,
    All = 1,
    Force = 2,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    pub id: u32,
    pub nzb_id: u32,
    pub filename: String,
    pub subject: String,
    pub output_filename: String,
    pub groups: Vec<String>,
    pub articles: Vec<ArticleInfo>,
    pub size: u64,
    pub remaining_size: u64,
    pub success_size: u64,
    pub failed_size: u64,
    pub missed_size: u64,
    pub total_articles: u32,
    pub missing_articles: u32,
    pub failed_articles: u32,
    pub success_articles: u32,
    pub paused: bool,
    pub completed: bool,
    pub priority: Priority,
    pub time: SystemTime,
    pub active_downloads: u32,
    pub crc: u32,
    pub server_stats: Vec<ServerStat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArticleInfo {
    pub part_number: u32,
    pub message_id: String,
    pub size: u64,
    pub status: ArticleStatus,
    pub segment_offset: u64,
    pub segment_size: u64,
    pub crc: u32,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
pub enum ArticleStatus {
    Undefined = 0,
    Running = 1,
    Finished = 2,
    Failed = 3,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadQueue {
    pub queue: Vec<NzbInfo>,
    pub history: Vec<HistoryInfo>,
    pub next_nzb_id: u32,
    pub next_file_id: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryInfo {
    pub id: u32,
    pub kind: HistoryKind,
    pub time: SystemTime,
    pub nzb_info: NzbInfo,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
pub enum HistoryKind {
    Nzb = 0,
    Url = 1,
    DupHidden = 2,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerStat {
    pub server_id: u32,
    pub success_articles: u32,
    pub failed_articles: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletedFile {
    pub filename: String,
    pub original_filename: String,
    pub status: CompletedFileStatus,
    pub crc: u32,
    pub id: u32,
    pub size: u64,
    pub hash: Option<String>,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
pub enum CompletedFileStatus {
    Success = 0,
    Failure = 1,
    Dupe = 2,
    Deleted = 3,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NzbParameter {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
pub enum PostStage {
    Queued = 0,
    ParLoading = 1,
    ParRenaming = 2,
    ParVerifying = 3,
    ParRepairing = 4,
    Unpacking = 5,
    Moving = 6,
    Executing = 7,
    Finished = 8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostMessage {
    pub kind: PostMessageKind,
    pub text: String,
    pub time: SystemTime,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
pub enum PostMessageKind {
    Info = 0,
    Warning = 1,
    Error = 2,
    Detail = 3,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueSnapshot {
    pub queue: Vec<NzbSummary>,
    pub history: Vec<HistorySummary>,
    pub download_rate: u64,
    pub remaining_size: u64,
    pub download_paused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistorySummary {
    pub id: u32,
    pub name: String,
    pub status: HistoryStatus,
    pub time: SystemTime,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
pub enum HistoryStatus {
    Success = 0,
    Failure = 1,
    Warning = 2,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PostStrategy {
    #[default]
    Sequential,
    Balanced,
    Rocket,
    Aggressive,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nzb_info_roundtrips_through_json() {
        let info = NzbInfo {
            id: 1,
            kind: NzbKind::Nzb,
            name: "Example".to_string(),
            filename: "example.nzb".to_string(),
            url: "".to_string(),
            dest_dir: PathBuf::from("/tmp/dest"),
            final_dir: PathBuf::from("/tmp/final"),
            temp_dir: PathBuf::from("/tmp/temp"),
            queue_dir: PathBuf::from("/tmp/queue"),
            category: "tv".to_string(),
            priority: Priority::High,
            dup_key: "".to_string(),
            dup_mode: DupMode::Score,
            dup_score: 0,
            size: 100,
            remaining_size: 80,
            paused_size: 0,
            failed_size: 0,
            success_size: 20,
            current_downloaded_size: 10,
            par_size: 0,
            par_remaining_size: 0,
            par_current_success_size: 0,
            par_failed_size: 0,
            file_count: 1,
            remaining_file_count: 1,
            remaining_par_count: 0,
            total_article_count: 2,
            success_article_count: 1,
            failed_article_count: 1,
            added_time: SystemTime::UNIX_EPOCH,
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
            par_status: ParStatus::None,
            unpack_status: UnpackStatus::None,
            move_status: MoveStatus::None,
            delete_status: DeleteStatus::None,
            mark_status: MarkStatus::None,
            url_status: UrlStatus::None,
            health: 1000,
            critical_health: 950,
            files: vec![],
            completed_files: vec![],
            server_stats: vec![],
            parameters: vec![],
            post_info: None,
            message_count: 0,
            cached_message_count: 0,
        };

        let value = serde_json::to_value(&info).expect("serialize");
        let decoded: NzbInfo = serde_json::from_value(value).expect("deserialize");
        assert_eq!(decoded.id, info.id);
        assert_eq!(decoded.kind, info.kind);
        assert_eq!(decoded.priority, info.priority);
        assert_eq!(decoded.par_status, info.par_status);
        assert_eq!(decoded.files.len(), info.files.len());
    }

    #[test]
    fn priority_ordering_matches_expected_weights() {
        let ordered = vec![
            Priority::VeryLow,
            Priority::Low,
            Priority::Normal,
            Priority::High,
            Priority::VeryHigh,
            Priority::Force,
        ];

        let mut sorted = ordered.clone();
        sorted.sort();

        assert_eq!(sorted, ordered);
    }

    #[test]
    fn queue_snapshot_serializes_basic_fields() {
        let snapshot = QueueSnapshot {
            queue: vec![NzbSummary {
                id: 42,
                name: "Example".to_string(),
                category: "tv".to_string(),
                size: 100,
                remaining_size: 50,
                paused: false,
                priority: Priority::Normal,
                health: 900,
                file_count: 2,
                remaining_file_count: 1,
            }],
            history: vec![HistorySummary {
                id: 99,
                name: "Done".to_string(),
                status: HistoryStatus::Success,
                time: SystemTime::UNIX_EPOCH,
            }],
            download_rate: 1000,
            remaining_size: 50,
            download_paused: false,
        };

        let value = serde_json::to_value(&snapshot).expect("serialize");
        assert_eq!(value["queue"][0]["id"], json!(42));
        assert_eq!(value["history"][0]["status"], json!(0));
    }
}
