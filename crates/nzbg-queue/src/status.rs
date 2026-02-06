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
}

#[derive(Debug, Clone)]
pub struct NzbListEntry {
    pub id: u32,
    pub name: String,
    pub priority: Priority,
}
