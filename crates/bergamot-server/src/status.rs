use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct NewsServerStatus {
    #[serde(rename = "ID")]
    pub id: u32,
    #[serde(rename = "Active")]
    pub active: bool,
}

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    #[serde(rename = "RemainingSizeLo")]
    pub remaining_size_lo: u32,
    #[serde(rename = "RemainingSizeHi")]
    pub remaining_size_hi: u32,
    #[serde(rename = "RemainingSizeMB")]
    pub remaining_size_mb: u64,
    #[serde(rename = "ForcedSizeLo")]
    pub forced_size_lo: u32,
    #[serde(rename = "ForcedSizeHi")]
    pub forced_size_hi: u32,
    #[serde(rename = "ForcedSizeMB")]
    pub forced_size_mb: u64,
    #[serde(rename = "DownloadedSizeLo")]
    pub downloaded_size_lo: u32,
    #[serde(rename = "DownloadedSizeHi")]
    pub downloaded_size_hi: u32,
    #[serde(rename = "DownloadedSizeMB")]
    pub downloaded_size_mb: u64,
    #[serde(rename = "MonthSizeLo")]
    pub month_size_lo: u32,
    #[serde(rename = "MonthSizeHi")]
    pub month_size_hi: u32,
    #[serde(rename = "MonthSizeMB")]
    pub month_size_mb: u64,
    #[serde(rename = "DaySizeLo")]
    pub day_size_lo: u32,
    #[serde(rename = "DaySizeHi")]
    pub day_size_hi: u32,
    #[serde(rename = "DaySizeMB")]
    pub day_size_mb: u64,
    #[serde(rename = "ArticleCacheLo")]
    pub article_cache_lo: u32,
    #[serde(rename = "ArticleCacheHi")]
    pub article_cache_hi: u32,
    #[serde(rename = "ArticleCacheMB")]
    pub article_cache_mb: u64,
    #[serde(rename = "DownloadRate")]
    pub download_rate: u64,
    #[serde(rename = "DownloadRateLo")]
    pub download_rate_lo: u32,
    #[serde(rename = "DownloadRateHi")]
    pub download_rate_hi: u32,
    #[serde(rename = "AverageDownloadRate")]
    pub average_download_rate: u64,
    #[serde(rename = "AverageDownloadRateLo")]
    pub average_download_rate_lo: u32,
    #[serde(rename = "AverageDownloadRateHi")]
    pub average_download_rate_hi: u32,
    #[serde(rename = "DownloadLimit")]
    pub download_limit: u64,
    #[serde(rename = "ThreadCount")]
    pub thread_count: u32,
    #[serde(rename = "PostJobCount")]
    pub post_job_count: u32,
    #[serde(rename = "ParJobCount")]
    pub par_job_count: u32,
    #[serde(rename = "UrlCount")]
    pub url_count: u32,
    #[serde(rename = "QueueScriptCount")]
    pub queue_script_count: u32,
    #[serde(rename = "UpTimeSec")]
    pub up_time_sec: u64,
    #[serde(rename = "DownloadTimeSec")]
    pub download_time_sec: u64,
    #[serde(rename = "ServerTime")]
    pub server_time: i64,
    #[serde(rename = "ResumeTime")]
    pub resume_time: i64,
    #[serde(rename = "DownloadPaused")]
    pub download_paused: bool,
    #[serde(rename = "ServerPaused")]
    pub server_paused: bool,
    #[serde(rename = "Download2Paused")]
    pub download2_paused: bool,
    #[serde(rename = "PostPaused")]
    pub post_paused: bool,
    #[serde(rename = "ScanPaused")]
    pub scan_paused: bool,
    #[serde(rename = "ServerStandBy")]
    pub server_stand_by: bool,
    #[serde(rename = "QuotaReached")]
    pub quota_reached: bool,
    #[serde(rename = "FeedActive")]
    pub feed_active: bool,
    #[serde(rename = "FreeDiskSpaceLo")]
    pub free_disk_space_lo: u32,
    #[serde(rename = "FreeDiskSpaceHi")]
    pub free_disk_space_hi: u32,
    #[serde(rename = "FreeDiskSpaceMB")]
    pub free_disk_space_mb: u64,
    #[serde(rename = "TotalDiskSpaceLo")]
    pub total_disk_space_lo: u32,
    #[serde(rename = "TotalDiskSpaceHi")]
    pub total_disk_space_hi: u32,
    #[serde(rename = "TotalDiskSpaceMB")]
    pub total_disk_space_mb: u64,
    #[serde(rename = "FreeInterDiskSpaceLo")]
    pub free_inter_disk_space_lo: u32,
    #[serde(rename = "FreeInterDiskSpaceHi")]
    pub free_inter_disk_space_hi: u32,
    #[serde(rename = "FreeInterDiskSpaceMB")]
    pub free_inter_disk_space_mb: u64,
    #[serde(rename = "TotalInterDiskSpaceLo")]
    pub total_inter_disk_space_lo: u32,
    #[serde(rename = "TotalInterDiskSpaceHi")]
    pub total_inter_disk_space_hi: u32,
    #[serde(rename = "TotalInterDiskSpaceMB")]
    pub total_inter_disk_space_mb: u64,
    #[serde(rename = "NewsServers")]
    pub news_servers: Vec<NewsServerStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeFields {
    pub lo: u32,
    pub hi: u32,
    pub mb: u64,
}

impl From<u64> for SizeFields {
    fn from(bytes: u64) -> Self {
        let hi = (bytes >> 32) as u32;
        let lo = (bytes & 0xFFFF_FFFF) as u32;
        Self {
            lo,
            hi,
            mb: bytes / (1024 * 1024),
        }
    }
}
