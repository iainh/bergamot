use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use bergamot_core::models::DupMode;
use bergamot_feed::FeedItemStatus;

pub trait StateFormat: Send + Sync {
    fn serialize<T: Serialize>(&self, value: &T) -> anyhow::Result<Vec<u8>>;
    fn deserialize<T: for<'de> Deserialize<'de>>(&self, data: &[u8]) -> anyhow::Result<T>;
    fn file_extension(&self) -> &str;
}

#[derive(Debug)]
pub struct JsonFormat;

impl StateFormat for JsonFormat {
    fn serialize<T: Serialize>(&self, value: &T) -> anyhow::Result<Vec<u8>> {
        Ok(serde_json::to_vec(value)?)
    }

    fn deserialize<T: for<'de> Deserialize<'de>>(&self, data: &[u8]) -> anyhow::Result<T> {
        Ok(serde_json::from_slice(data)?)
    }

    fn file_extension(&self) -> &str {
        "json"
    }
}

#[derive(Debug)]
pub struct DiskState<F: StateFormat> {
    state_dir: PathBuf,
    format: F,
}

impl<F: StateFormat> DiskState<F> {
    pub fn new(state_dir: PathBuf, format: F) -> anyhow::Result<Self> {
        fs::create_dir_all(&state_dir)?;
        fs::create_dir_all(state_dir.join("nzb"))?;
        fs::create_dir_all(state_dir.join("file"))?;
        Ok(Self { state_dir, format })
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    pub fn save_queue(&self, queue: &QueueState) -> anyhow::Result<()> {
        let data = self.format.serialize(queue)?;
        let path = self
            .state_dir
            .join(format!("queue.{}", self.format.file_extension()));
        atomic_write(&path, &data)
    }

    pub fn load_queue(&self) -> anyhow::Result<QueueState> {
        let path = self
            .state_dir
            .join(format!("queue.{}", self.format.file_extension()));
        let data = fs::read(&path)?;
        let state: QueueState = self.format.deserialize(&data)?;
        state.migrate()
    }

    pub fn save_file_state(&self, file_id: u32, state: &FileArticleState) -> anyhow::Result<()> {
        let data = self.format.serialize(state)?;
        let path = self.state_dir.join(format!("file/{file_id:08}.state"));
        atomic_write(&path, &data)
    }

    pub fn wal_path(&self, file_id: u32) -> PathBuf {
        self.state_dir.join(format!("file/{file_id:08}.wal"))
    }

    pub fn append_file_wal(&self, file_id: u32, entries: &[(u32, u32)]) -> anyhow::Result<()> {
        use std::io::Write;
        let path = self.wal_path(file_id);
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        for &(article_index, crc) in entries {
            file.write_all(&article_index.to_le_bytes())?;
            file.write_all(&crc.to_le_bytes())?;
        }
        Ok(())
    }

    pub fn load_file_state(&self, file_id: u32) -> anyhow::Result<FileArticleState> {
        let path = self.state_dir.join(format!("file/{file_id:08}.state"));
        let data = fs::read(&path)?;
        let mut state: FileArticleState = self.format.deserialize(&data)?;

        let wal_path = self.wal_path(file_id);
        if wal_path.exists() {
            let wal_data = fs::read(&wal_path)?;
            let record_size = 8;
            let full_records = wal_data.len() / record_size;
            for i in 0..full_records {
                let offset = i * record_size;
                let article_index =
                    u32::from_le_bytes(wal_data[offset..offset + 4].try_into().unwrap());
                let crc = u32::from_le_bytes(wal_data[offset + 4..offset + 8].try_into().unwrap());
                state.mark_article_done(article_index, crc);
            }
        }

        Ok(state)
    }

    pub fn compact_file_state(&self, file_id: u32, state: &FileArticleState) -> anyhow::Result<()> {
        self.save_file_state(file_id, state)?;
        let wal_path = self.wal_path(file_id);
        match fs::remove_file(&wal_path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    pub fn delete_file_state(&self, file_id: u32) -> anyhow::Result<()> {
        let path = self.state_dir.join(format!("file/{file_id:08}.state"));
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
        let wal_path = self.wal_path(file_id);
        match fs::remove_file(&wal_path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    pub fn save_history(&self, history: &HistoryState) -> anyhow::Result<()> {
        let data = self.format.serialize(history)?;
        let path = self
            .state_dir
            .join(format!("history.{}", self.format.file_extension()));
        atomic_write(&path, &data)
    }

    pub fn load_history(&self) -> anyhow::Result<HistoryState> {
        let path = self
            .state_dir
            .join(format!("history.{}", self.format.file_extension()));
        let data = fs::read(&path)?;
        self.format.deserialize(&data)
    }

    pub fn save_stats(&self, stats: &ServerStats) -> anyhow::Result<()> {
        let data = self.format.serialize(stats)?;
        let path = self
            .state_dir
            .join(format!("stats.{}", self.format.file_extension()));
        atomic_write(&path, &data)
    }

    pub fn load_stats(&self) -> anyhow::Result<ServerStats> {
        let path = self
            .state_dir
            .join(format!("stats.{}", self.format.file_extension()));
        let data = fs::read(&path)?;
        self.format.deserialize(&data)
    }

    pub fn save_feeds(&self, feeds: &FeedHistoryState) -> anyhow::Result<()> {
        let data = self.format.serialize(feeds)?;
        let path = self
            .state_dir
            .join(format!("feeds.{}", self.format.file_extension()));
        atomic_write(&path, &data)
    }

    pub fn load_feeds(&self) -> anyhow::Result<FeedHistoryState> {
        let path = self
            .state_dir
            .join(format!("feeds.{}", self.format.file_extension()));
        let data = fs::read(&path)?;
        self.format.deserialize(&data)
    }

    pub fn save_nzb_file(&self, id: u32, content: &[u8]) -> anyhow::Result<()> {
        let path = self.state_dir.join(format!("nzb/{id:08}.nzb"));
        atomic_write(&path, content)
    }

    pub fn load_nzb_file(&self, id: u32) -> anyhow::Result<Vec<u8>> {
        let path = self.state_dir.join(format!("nzb/{id:08}.nzb"));
        Ok(fs::read(&path)?)
    }

    pub fn delete_nzb_file(&self, id: u32) -> anyhow::Result<()> {
        let path = self.state_dir.join(format!("nzb/{id:08}.nzb"));
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    pub fn recover(&self) -> anyhow::Result<RecoveryReport> {
        let mut report = RecoveryReport::default();
        let cleanup_dirs = [
            self.state_dir.clone(),
            self.state_dir.join("file"),
            self.state_dir.join("nzb"),
        ];
        for dir in &cleanup_dirs {
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.flatten() {
                    if entry.path().extension() == Some("tmp".as_ref()) {
                        fs::remove_file(entry.path())?;
                        report.tmp_files_cleaned += 1;
                    }
                }
            }
        }
        Ok(report)
    }

    pub fn validate_consistency(
        &self,
        queue: &QueueState,
    ) -> anyhow::Result<Vec<ConsistencyWarning>> {
        let mut warnings = Vec::new();

        for nzb in &queue.nzbs {
            let nzb_path = self.state_dir.join(format!("nzb/{:08}.nzb", nzb.id));
            if !nzb_path.exists() {
                warnings.push(ConsistencyWarning::MissingNzbFile { nzb_id: nzb.id });
            }

            if nzb.success_article_count > 0 || nzb.failed_article_count > 0 {
                for &file_id in &nzb.file_ids {
                    let state_path = self.state_dir.join(format!("file/{file_id:08}.state"));
                    let wal_path = self.wal_path(file_id);
                    if !state_path.exists() && !wal_path.exists() {
                        warnings.push(ConsistencyWarning::MissingFileState { file_id });
                    }
                }
            }
        }

        let file_dir = self.state_dir.join("file");
        if let Ok(entries) = fs::read_dir(&file_dir) {
            let known_ids: HashSet<u32> = queue
                .nzbs
                .iter()
                .flat_map(|nzb| nzb.file_ids.iter().copied())
                .collect();

            for entry in entries.flatten() {
                if let Some(id) = parse_file_id(&entry.path())
                    && !known_ids.contains(&id)
                {
                    warnings.push(ConsistencyWarning::OrphanedFileState { file_id: id });
                }
            }
        }

        Ok(warnings)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueState {
    pub version: u32,
    pub nzbs: Vec<NzbState>,
    pub next_nzb_id: u32,
    pub next_file_id: u32,
    pub download_paused: bool,
    pub speed_limit: u64,
}

impl QueueState {
    pub fn migrate(mut self) -> anyhow::Result<Self> {
        loop {
            match self.version {
                1 => {
                    for nzb in &mut self.nzbs {
                        nzb.dupe_mode = DupMode::Score;
                    }
                    self.version = 2;
                }
                2 => {
                    for nzb in &mut self.nzbs {
                        if nzb.health == 0 && nzb.total_article_count > 0 {
                            nzb.health = 1000;
                        }
                    }
                    self.version = 3;
                }
                3 => return Ok(self),
                version => anyhow::bail!("unsupported state version: {version}"),
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NzbState {
    pub id: u32,
    pub name: String,
    pub filename: String,
    pub category: String,
    pub dest_dir: PathBuf,
    pub final_dir: Option<PathBuf>,
    pub priority: i32,
    pub paused: bool,
    pub url: Option<String>,
    pub dupe_key: String,
    pub dupe_score: i32,
    pub dupe_mode: DupMode,
    pub added_time: DateTime<Utc>,
    pub total_size: u64,
    pub downloaded_size: u64,
    pub failed_size: u64,
    pub file_ids: Vec<u32>,
    pub post_process_parameters: HashMap<String, String>,
    pub health: u32,
    pub critical_health: u32,
    pub total_article_count: u32,
    pub success_article_count: u32,
    pub failed_article_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryState {
    pub version: u32,
    pub entries: Vec<HistoryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: u32,
    pub name: String,
    pub status: String,
    pub completed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerStats {
    pub version: u32,
    pub servers: Vec<ServerVolumeStat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerVolumeStat {
    pub server_id: u32,
    pub server_name: String,
    pub daily_bytes: HashMap<NaiveDate, u64>,
    pub monthly_bytes: HashMap<String, u64>,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedHistoryState {
    pub version: u32,
    pub feeds: HashMap<u32, Vec<FeedHistoryItem>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedHistoryItem {
    pub url: String,
    pub status: FeedItemStatus,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileArticleState {
    pub file_id: u32,
    pub total_articles: u32,
    pub completed: Vec<u8>,
    pub article_crcs: HashMap<u32, u32>,
}

impl FileArticleState {
    pub fn new(file_id: u32, total_articles: u32) -> Self {
        let byte_count = (total_articles.div_ceil(8)) as usize;
        Self {
            file_id,
            total_articles,
            completed: vec![0u8; byte_count],
            article_crcs: HashMap::new(),
        }
    }

    pub fn is_article_done(&self, index: u32) -> bool {
        let byte = (index / 8) as usize;
        let bit = index % 8;
        self.completed
            .get(byte)
            .is_some_and(|value| value & (1 << bit) != 0)
    }

    pub fn mark_article_done(&mut self, index: u32, crc: u32) {
        let byte = (index / 8) as usize;
        let bit = index % 8;
        if byte >= self.completed.len() {
            self.completed.resize(byte + 1, 0);
        }
        self.completed[byte] |= 1 << bit;
        self.article_crcs.insert(index, crc);
    }

    pub fn completed_count(&self) -> u32 {
        self.completed.iter().map(|value| value.count_ones()).sum()
    }

    pub fn remaining_count(&self) -> u32 {
        self.total_articles - self.completed_count()
    }
}

#[derive(Debug, Clone, Default)]
pub struct RecoveryReport {
    pub tmp_files_cleaned: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsistencyWarning {
    MissingNzbFile { nzb_id: u32 },
    MissingFileState { file_id: u32 },
    OrphanedFileState { file_id: u32 },
}

pub struct StateLock {
    _lock_file: fs::File,
}

impl StateLock {
    pub fn acquire(state_dir: &Path) -> anyhow::Result<Self> {
        fs::create_dir_all(state_dir)?;
        let lock_path = state_dir.join("diskstate.lock");
        let lock_file = fs::File::create(&lock_path)?;
        lock_file.try_lock_exclusive().map_err(|_| {
            anyhow::anyhow!(
                "another bergamot instance is already running (lock held on {})",
                lock_path.display()
            )
        })?;
        Ok(Self {
            _lock_file: lock_file,
        })
    }
}

pub struct AtomicWriteOptions {
    pub sync_file: bool,
    pub sync_dir: bool,
}

pub fn atomic_write_with_options(
    path: &Path,
    data: &[u8],
    opt: AtomicWriteOptions,
) -> anyhow::Result<()> {
    let tmp_path = path.with_extension("tmp");

    let mut file = fs::File::create(&tmp_path)?;
    file.write_all(data)?;
    if opt.sync_file {
        file.sync_all()?;
    }

    fs::rename(&tmp_path, path)?;

    if opt.sync_dir
        && let Some(parent) = path.parent()
    {
        let dir = fs::File::open(parent)?;
        dir.sync_all()?;
    }

    Ok(())
}

pub fn atomic_write_relaxed(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    atomic_write_with_options(
        path,
        data,
        AtomicWriteOptions {
            sync_file: false,
            sync_dir: false,
        },
    )
}

pub fn atomic_write(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    let tmp_path = path.with_extension("tmp");

    let mut file = fs::File::create(&tmp_path)?;
    file.write_all(data)?;
    file.sync_all()?;

    fs::rename(&tmp_path, path)?;

    if let Some(parent) = path.parent() {
        let dir = fs::File::open(parent)?;
        dir.sync_all()?;
    }

    Ok(())
}

fn parse_file_id(path: &Path) -> Option<u32> {
    let stem = path.file_stem()?.to_string_lossy();
    stem.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::TempDir;

    fn sample_queue_state() -> QueueState {
        QueueState {
            version: 3,
            nzbs: vec![NzbState {
                id: 1,
                name: "sample".to_string(),
                filename: "sample.nzb".to_string(),
                category: "tv".to_string(),
                dest_dir: PathBuf::from("/dest"),
                final_dir: None,
                priority: 0,
                paused: false,
                url: None,
                dupe_key: String::new(),
                dupe_score: 0,
                dupe_mode: DupMode::Score,
                added_time: Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
                total_size: 0,
                downloaded_size: 0,
                failed_size: 0,
                file_ids: vec![10, 11],
                post_process_parameters: HashMap::new(),
                health: 1000,
                critical_health: 1000,
                total_article_count: 0,
                success_article_count: 0,
                failed_article_count: 0,
            }],
            next_nzb_id: 2,
            next_file_id: 12,
            download_paused: false,
            speed_limit: 0,
        }
    }

    #[test]
    fn file_article_state_tracks_bits() {
        let mut state = FileArticleState::new(1, 10);
        assert!(!state.is_article_done(3));
        state.mark_article_done(3, 1234);
        assert!(state.is_article_done(3));
        assert_eq!(state.completed_count(), 1);
        assert_eq!(state.remaining_count(), 9);
    }

    #[test]
    fn disk_state_roundtrip_queue() {
        let temp = TempDir::new().expect("temp dir");
        let disk_state = DiskState::new(temp.path().to_path_buf(), JsonFormat).expect("disk state");
        let queue = sample_queue_state();
        disk_state.save_queue(&queue).expect("save");
        let loaded = disk_state.load_queue().expect("load");
        assert_eq!(loaded.nzbs[0].name, "sample");
    }

    #[test]
    fn disk_state_recovers_tmp_files() {
        let temp = TempDir::new().expect("temp dir");
        let disk_state = DiskState::new(temp.path().to_path_buf(), JsonFormat).expect("disk state");
        let tmp_path = disk_state.state_dir.join("queue.tmp");
        fs::write(&tmp_path, b"data").expect("tmp write");
        let report = disk_state.recover().expect("recover");
        assert_eq!(report.tmp_files_cleaned, 1);
        assert!(!tmp_path.exists());
    }

    #[test]
    fn disk_state_roundtrip_feeds() {
        let temp = TempDir::new().expect("temp dir");
        let disk_state = DiskState::new(temp.path().to_path_buf(), JsonFormat).expect("disk state");

        let now = Utc::now();
        let feed_state = FeedHistoryState {
            version: 1,
            feeds: {
                let mut m = HashMap::new();
                m.insert(
                    1,
                    vec![FeedHistoryItem {
                        url: "https://example.com/1.nzb".to_string(),
                        status: FeedItemStatus::Fetched,
                        first_seen: now,
                        last_seen: now,
                    }],
                );
                m
            },
        };
        disk_state.save_feeds(&feed_state).expect("save feeds");
        let loaded = disk_state.load_feeds().expect("load feeds");
        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.feeds.get(&1).unwrap().len(), 1);
        assert_eq!(
            loaded.feeds.get(&1).unwrap()[0].url,
            "https://example.com/1.nzb"
        );
    }

    #[test]
    fn atomic_write_with_options_relaxed_writes_correct_contents() {
        let temp = TempDir::new().expect("temp dir");
        let path = temp.path().join("relaxed.dat");
        let data = b"relaxed write test data";
        atomic_write_with_options(
            &path,
            data,
            AtomicWriteOptions {
                sync_file: false,
                sync_dir: false,
            },
        )
        .expect("write");
        let read_back = fs::read(&path).expect("read");
        assert_eq!(read_back, data);
    }

    #[test]
    fn atomic_write_relaxed_writes_correct_contents() {
        let temp = TempDir::new().expect("temp dir");
        let path = temp.path().join("relaxed2.dat");
        let data = b"relaxed shorthand";
        atomic_write_relaxed(&path, data).expect("write");
        let read_back = fs::read(&path).expect("read");
        assert_eq!(read_back, data);
    }

    #[test]
    fn wal_roundtrip_replays_entries_on_load() {
        let temp = TempDir::new().expect("temp dir");
        let disk_state = DiskState::new(temp.path().to_path_buf(), JsonFormat).expect("disk state");

        let mut state = FileArticleState::new(1, 100);
        state.mark_article_done(0, 0xAAAA);
        disk_state
            .save_file_state(1, &state)
            .expect("save snapshot");

        disk_state
            .append_file_wal(1, &[(5, 0xBBBB), (10, 0xCCCC)])
            .expect("append wal");

        let loaded = disk_state.load_file_state(1).expect("load");
        assert!(loaded.is_article_done(0));
        assert!(loaded.is_article_done(5));
        assert!(loaded.is_article_done(10));
        assert_eq!(loaded.article_crcs[&0], 0xAAAA);
        assert_eq!(loaded.article_crcs[&5], 0xBBBB);
        assert_eq!(loaded.article_crcs[&10], 0xCCCC);
    }

    #[test]
    fn wal_partial_tail_is_ignored() {
        let temp = TempDir::new().expect("temp dir");
        let disk_state = DiskState::new(temp.path().to_path_buf(), JsonFormat).expect("disk state");

        let state = FileArticleState::new(1, 100);
        disk_state
            .save_file_state(1, &state)
            .expect("save snapshot");

        let wal_path = disk_state.wal_path(1);
        let mut data = Vec::new();
        data.extend_from_slice(&3u32.to_le_bytes());
        data.extend_from_slice(&0xDDDDu32.to_le_bytes());
        data.push(0xFF); // partial trailing byte
        fs::write(&wal_path, &data).expect("write wal");

        let loaded = disk_state.load_file_state(1).expect("load");
        assert!(loaded.is_article_done(3));
        assert_eq!(loaded.article_crcs[&3], 0xDDDD);
        assert_eq!(loaded.completed_count(), 1);
    }

    #[test]
    fn delete_file_state_removes_wal() {
        let temp = TempDir::new().expect("temp dir");
        let disk_state = DiskState::new(temp.path().to_path_buf(), JsonFormat).expect("disk state");

        let state = FileArticleState::new(1, 10);
        disk_state.save_file_state(1, &state).expect("save");
        disk_state
            .append_file_wal(1, &[(1, 0x1111)])
            .expect("append");

        assert!(disk_state.wal_path(1).exists());
        disk_state.delete_file_state(1).expect("delete");
        assert!(!disk_state.wal_path(1).exists());
        let state_path = disk_state.state_dir.join("file/00000001.state");
        assert!(!state_path.exists());
    }

    #[test]
    fn compact_file_state_writes_snapshot_and_removes_wal() {
        let temp = TempDir::new().expect("temp dir");
        let disk_state = DiskState::new(temp.path().to_path_buf(), JsonFormat).expect("disk state");

        let mut state = FileArticleState::new(1, 100);
        state.mark_article_done(0, 0xAAAA);
        disk_state.save_file_state(1, &state).expect("save");
        disk_state
            .append_file_wal(1, &[(5, 0xBBBB)])
            .expect("append");

        let full_state = disk_state.load_file_state(1).expect("load");
        disk_state
            .compact_file_state(1, &full_state)
            .expect("compact");

        assert!(!disk_state.wal_path(1).exists());
        let reloaded = disk_state.load_file_state(1).expect("reload");
        assert!(reloaded.is_article_done(0));
        assert!(reloaded.is_article_done(5));
        assert_eq!(reloaded.article_crcs[&5], 0xBBBB);
    }

    #[test]
    fn validate_consistency_accepts_wal_only_presence() {
        let temp = TempDir::new().expect("temp dir");
        let disk_state = DiskState::new(temp.path().to_path_buf(), JsonFormat).expect("disk state");

        let mut queue = sample_queue_state();
        queue.nzbs[0].success_article_count = 1;

        let state = FileArticleState::new(10, 10);
        disk_state.save_file_state(10, &state).expect("save");

        disk_state
            .append_file_wal(11, &[(0, 0x1111)])
            .expect("append wal for file 11");

        let warnings = disk_state.validate_consistency(&queue).expect("validate");
        assert!(
            !warnings
                .iter()
                .any(|w| matches!(w, ConsistencyWarning::MissingFileState { file_id: 11 })),
            "file 11 has WAL so should not be reported missing"
        );
    }

    #[test]
    fn disk_state_detects_orphaned_file_states() {
        let temp = TempDir::new().expect("temp dir");
        let disk_state = DiskState::new(temp.path().to_path_buf(), JsonFormat).expect("disk state");
        let queue = sample_queue_state();
        let file_dir = disk_state.state_dir.join("file");
        fs::create_dir_all(&file_dir).expect("file dir");
        fs::write(file_dir.join("00000099.state"), b"{}").expect("write");

        let warnings = disk_state.validate_consistency(&queue).expect("warnings");
        assert!(warnings.iter().any(|warning| {
            matches!(
                warning,
                ConsistencyWarning::OrphanedFileState { file_id: 99 }
            )
        }));
    }
}
