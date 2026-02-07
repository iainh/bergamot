use crate::format::SliceChecksumEntry;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId(pub [u8; 16]);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Md5Digest(pub [u8; 16]);

#[derive(Debug, Clone)]
pub struct Par2FileEntry {
    pub file_id: FileId,
    pub filename: String,
    pub length: u64,
    pub hash_full: Md5Digest,
    pub hash_16k: Md5Digest,
    pub slice_checksums: Vec<SliceChecksumEntry>,
}

#[derive(Debug, Clone)]
pub struct RecoverySet {
    pub set_id: [u8; 16],
    pub slice_size: u64,
    pub files: Vec<Par2FileEntry>,
    pub recovery_slice_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileVerifyStatus {
    Ok,
    Missing,
    Damaged { bad_slices: Vec<u32> },
}

#[derive(Debug, Clone)]
pub struct VerifyResult {
    pub files: Vec<FileVerifyResult>,
}

#[derive(Debug, Clone)]
pub struct FileVerifyResult {
    pub file_id: FileId,
    pub filename: String,
    pub status: FileVerifyStatus,
    pub slice_count: u32,
}

impl VerifyResult {
    pub fn all_ok(&self) -> bool {
        self.files.iter().all(|f| f.status == FileVerifyStatus::Ok)
    }

    pub fn blocks_needed(&self) -> usize {
        self.files
            .iter()
            .map(|f| match &f.status {
                FileVerifyStatus::Ok => 0,
                FileVerifyStatus::Missing => f.slice_count as usize,
                FileVerifyStatus::Damaged { bad_slices } => bad_slices.len(),
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_result_all_ok_when_all_files_ok() {
        let result = VerifyResult {
            files: vec![
                FileVerifyResult {
                    file_id: FileId([0; 16]),
                    filename: "a.rar".into(),
                    status: FileVerifyStatus::Ok,
                    slice_count: 10,
                },
                FileVerifyResult {
                    file_id: FileId([1; 16]),
                    filename: "b.rar".into(),
                    status: FileVerifyStatus::Ok,
                    slice_count: 5,
                },
            ],
        };
        assert!(result.all_ok());
        assert_eq!(result.blocks_needed(), 0);
    }

    #[test]
    fn blocks_needed_counts_missing_file_slices() {
        let result = VerifyResult {
            files: vec![FileVerifyResult {
                file_id: FileId([0; 16]),
                filename: "gone.rar".into(),
                status: FileVerifyStatus::Missing,
                slice_count: 8,
            }],
        };
        assert!(!result.all_ok());
        assert_eq!(result.blocks_needed(), 8);
    }

    #[test]
    fn blocks_needed_counts_damaged_slices() {
        let result = VerifyResult {
            files: vec![FileVerifyResult {
                file_id: FileId([0; 16]),
                filename: "bad.rar".into(),
                status: FileVerifyStatus::Damaged {
                    bad_slices: vec![2, 5, 7],
                },
                slice_count: 10,
            }],
        };
        assert_eq!(result.blocks_needed(), 3);
    }
}
