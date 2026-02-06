use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    #[serde(rename = "RemainingSizeLo")]
    pub remaining_size_lo: u32,
    #[serde(rename = "RemainingSizeHi")]
    pub remaining_size_hi: u32,
    #[serde(rename = "RemainingSizeMB")]
    pub remaining_size_mb: u64,
    #[serde(rename = "DownloadRate")]
    pub download_rate: u64,
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
