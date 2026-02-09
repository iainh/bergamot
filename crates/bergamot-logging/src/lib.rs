mod tracing_layer;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use bergamot_core::models::{DeleteStatus, DupMode};

pub use crate::tracing_layer::BufferLayer;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LogLevel {
    Debug = 0,
    Detail = 1,
    Info = 2,
    Warning = 3,
    Error = 4,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogMessage {
    pub id: u32,
    pub kind: LogLevel,
    pub time: DateTime<Utc>,
    pub text: String,
    pub nzb_id: Option<u32>,
}

#[derive(Debug)]
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

    pub fn push(&self, mut message: LogMessage) {
        message.id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut messages = self.messages.lock().expect("log buffer lock");
        if messages.len() >= self.capacity {
            messages.pop_front();
        }
        messages.push_back(message);
    }

    pub fn clear(&self) {
        let mut messages = self.messages.lock().expect("log buffer lock");
        messages.clear();
    }

    pub fn messages_since(&self, since_id: u32) -> Vec<LogMessage> {
        let messages = self.messages.lock().expect("log buffer lock");
        messages
            .iter()
            .filter(|message| message.id > since_id)
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TotalStatus {
    Success,
    Warning,
    Failure,
    Deleted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ParStatus {
    None,
    Failure,
    Success,
    RepairPossible,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum UnpackStatus {
    None,
    Failure,
    Success,
    PasswordRequired,
    Space,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum HistoryMark {
    None,
    Bad,
    Good,
    Success,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptStatus {
    pub name: String,
    pub exit_code: i32,
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

    pub status: String,
    pub total_status: TotalStatus,
    pub par_status: ParStatus,
    pub unpack_status: UnpackStatus,
    pub script_statuses: Vec<ScriptStatus>,

    pub added_time: DateTime<Utc>,
    pub completed_time: DateTime<Utc>,
    pub download_time_sec: u64,
    pub post_process_time_sec: u64,

    pub size: u64,
    pub downloaded_size: u64,
    pub file_count: u32,
    pub article_count: u32,
    pub failed_articles: u32,
    pub health: f32,

    pub dupe_key: String,
    pub dupe_score: i32,
    pub dupe_mode: DupMode,

    pub delete_status: DeleteStatus,
    pub mark: HistoryMark,
}

#[derive(Debug)]
pub struct HistoryCoordinator {
    entries: Vec<HistoryEntry>,
    keep_history_days: u32,
}

impl HistoryCoordinator {
    pub fn new(keep_history_days: u32) -> Self {
        Self {
            entries: Vec::new(),
            keep_history_days,
        }
    }

    pub fn add_entry(&mut self, entry: HistoryEntry) {
        self.entries.push(entry);
        self.purge_expired();
    }

    pub fn return_to_queue(&mut self, id: u32) -> Option<HistoryEntry> {
        let index = self.entries.iter().position(|entry| entry.id == id)?;
        Some(self.entries.remove(index))
    }

    pub fn mark(&mut self, id: u32, mark: HistoryMark) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) {
            entry.mark = mark;
        }
    }

    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    fn purge_expired(&mut self) {
        if self.keep_history_days == 0 {
            return;
        }
        let cutoff = Utc::now() - chrono::Duration::days(self.keep_history_days as i64);
        self.entries.retain(|entry| entry.completed_time > cutoff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn log_buffer_tracks_recent_messages() {
        let buffer = LogBuffer::new(2);
        buffer.push(LogMessage {
            id: 0,
            kind: LogLevel::Info,
            time: Utc::now(),
            text: "first".to_string(),
            nzb_id: None,
        });
        buffer.push(LogMessage {
            id: 0,
            kind: LogLevel::Info,
            time: Utc::now(),
            text: "second".to_string(),
            nzb_id: None,
        });
        buffer.push(LogMessage {
            id: 0,
            kind: LogLevel::Info,
            time: Utc::now(),
            text: "third".to_string(),
            nzb_id: None,
        });

        let messages = buffer.messages_since(0);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].text, "second");
        assert_eq!(messages[1].text, "third");
    }

    fn sample_history_entry(id: u32, days_ago: i64) -> HistoryEntry {
        HistoryEntry {
            id,
            nzb_name: format!("nzb-{id}"),
            nzb_filename: format!("nzb-{id}.nzb"),
            category: "tv".to_string(),
            dest_dir: PathBuf::from("/dest"),
            final_dir: None,
            url: None,
            status: "SUCCESS".to_string(),
            total_status: TotalStatus::Success,
            par_status: ParStatus::Success,
            unpack_status: UnpackStatus::Success,
            script_statuses: vec![],
            added_time: Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
            completed_time: Utc::now() - chrono::Duration::days(days_ago),
            download_time_sec: 10,
            post_process_time_sec: 20,
            size: 100,
            downloaded_size: 100,
            file_count: 1,
            article_count: 10,
            failed_articles: 0,
            health: 1000.0,
            dupe_key: String::new(),
            dupe_score: 0,
            dupe_mode: DupMode::Score,
            delete_status: DeleteStatus::None,
            mark: HistoryMark::None,
        }
    }

    #[test]
    fn history_coordinator_purges_entries() {
        let mut coordinator = HistoryCoordinator::new(1);
        coordinator.add_entry(sample_history_entry(1, 2));
        coordinator.add_entry(sample_history_entry(2, 0));
        assert_eq!(coordinator.entries().len(), 1);
        assert_eq!(coordinator.entries()[0].id, 2);
    }

    #[test]
    fn history_coordinator_returns_to_queue() {
        let mut coordinator = HistoryCoordinator::new(0);
        coordinator.add_entry(sample_history_entry(1, 0));
        coordinator.add_entry(sample_history_entry(2, 0));
        let entry = coordinator.return_to_queue(1).expect("entry");
        assert_eq!(entry.id, 1);
        assert_eq!(coordinator.entries().len(), 1);
    }
}
