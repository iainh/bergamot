use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub number: u32,
    pub bytes: u64,
    pub message_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NzbFile {
    pub poster: String,
    pub date: i64,
    pub subject: String,
    pub filename: Option<String>,
    pub groups: Vec<String>,
    pub segments: Vec<Segment>,
    pub par_status: ParStatus,
    pub total_size: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NzbMeta {
    pub title: Option<String>,
    pub password: Option<String>,
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub extra: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NzbInfo {
    pub meta: NzbMeta,
    pub files: Vec<NzbFile>,
    pub total_size: u64,
    pub file_count: usize,
    pub total_segments: usize,
    pub content_hash: u32,
    pub name_hash: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParStatus {
    NotPar,
    MainPar,
    RepairVolume { block_offset: u32, block_count: u32 },
}
