use std::path::Path;

use rayon::prelude::*;

use crate::model::{FileVerifyResult, FileVerifyStatus, Par2FileEntry, RecoverySet, VerifyResult};

pub fn verify_recovery_set(rs: &RecoverySet, working_dir: &Path) -> VerifyResult {
    let files: Vec<FileVerifyResult> = rs
        .files
        .par_iter()
        .map(|entry| verify_one_entry(entry, rs.slice_size, working_dir))
        .collect();

    VerifyResult { files }
}

fn verify_one_entry(
    entry: &Par2FileEntry,
    slice_size: u64,
    working_dir: &Path,
) -> FileVerifyResult {
    let slice_count = if slice_size > 0 {
        entry.length.div_ceil(slice_size) as u32
    } else {
        0
    };

    let file_path = working_dir.join(&entry.filename);

    if !file_path.is_file() {
        return FileVerifyResult {
            file_id: entry.file_id,
            filename: entry.filename.clone(),
            status: FileVerifyStatus::Missing,
            slice_count,
        };
    }

    let status = match verify_file(&file_path, entry, slice_size) {
        Ok(s) => s,
        Err(_) => FileVerifyStatus::Damaged {
            bad_slices: (0..slice_count).collect(),
        },
    };

    FileVerifyResult {
        file_id: entry.file_id,
        filename: entry.filename.clone(),
        status,
        slice_count,
    }
}

fn verify_file(
    path: &Path,
    entry: &crate::model::Par2FileEntry,
    slice_size: u64,
) -> Result<FileVerifyStatus, std::io::Error> {
    use digest::Digest;
    use std::io::Read;

    let metadata = std::fs::metadata(path)?;
    if metadata.len() != entry.length {
        let slice_count = if slice_size > 0 {
            entry.length.div_ceil(slice_size) as u32
        } else {
            0
        };
        return Ok(FileVerifyStatus::Damaged {
            bad_slices: (0..slice_count).collect(),
        });
    }

    let mut file = std::fs::File::open(path)?;

    let mut full_hasher = md5::Md5::new();
    let mut bad_slices = Vec::new();
    let mut remaining = entry.length;
    let mut slice_buf = vec![0u8; slice_size as usize];

    for (idx, expected) in entry.slice_checksums.iter().enumerate() {
        let this_slice = std::cmp::min(remaining, slice_size);
        let buf = &mut slice_buf[..this_slice as usize];
        file.read_exact(buf)?;

        full_hasher.update(&*buf);

        let mut slice_hasher = md5::Md5::new();
        slice_hasher.update(&*buf);
        if this_slice < slice_size {
            let padding = vec![0u8; (slice_size - this_slice) as usize];
            slice_hasher.update(&padding);
        }
        let slice_md5: [u8; 16] = slice_hasher.finalize().into();

        let crc = crc32fast::hash(&*buf);
        let padded_crc = if this_slice < slice_size {
            let padding = vec![0u8; (slice_size - this_slice) as usize];
            let mut h = crc32fast::Hasher::new_with_initial(crc);
            h.update(&padding);
            h.finalize()
        } else {
            crc
        };

        if slice_md5 != expected.md5 || padded_crc != expected.crc32 {
            bad_slices.push(idx as u32);
        }

        remaining -= this_slice;
    }

    if !bad_slices.is_empty() {
        return Ok(FileVerifyStatus::Damaged { bad_slices });
    }

    let full_md5: [u8; 16] = full_hasher.finalize().into();
    if full_md5 != entry.hash_full.0 {
        return Ok(FileVerifyStatus::Damaged { bad_slices: vec![] });
    }

    Ok(FileVerifyStatus::Ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::SliceChecksumEntry;
    use crate::model::{FileId, Md5Digest, Par2FileEntry};
    use digest::Digest;

    fn compute_md5(data: &[u8]) -> [u8; 16] {
        let mut h = md5::Md5::new();
        h.update(data);
        h.finalize().into()
    }

    fn make_entry(filename: &str, data: &[u8], slice_size: u64) -> Par2FileEntry {
        let full_md5 = compute_md5(data);
        let hash_16k = compute_md5(&data[..std::cmp::min(data.len(), 16384)]);

        let mut slices = Vec::new();
        let mut offset = 0usize;
        while offset < data.len() {
            let end = std::cmp::min(offset + slice_size as usize, data.len());
            let chunk = &data[offset..end];

            let mut slice_data = chunk.to_vec();
            if slice_data.len() < slice_size as usize {
                slice_data.resize(slice_size as usize, 0);
            }

            let md5 = compute_md5(&slice_data);
            let crc32 = crc32fast::hash(&slice_data);

            slices.push(SliceChecksumEntry { md5, crc32 });
            offset = end;
        }

        Par2FileEntry {
            file_id: FileId([0xAA; 16]),
            filename: filename.to_string(),
            length: data.len() as u64,
            hash_full: Md5Digest(full_md5),
            hash_16k: Md5Digest(hash_16k),
            slice_checksums: slices,
        }
    }

    #[test]
    fn verify_ok_when_file_matches() {
        let dir = tempfile::tempdir().unwrap();
        let data = b"Hello, PAR2 world! This is test data for verification.";
        std::fs::write(dir.path().join("test.dat"), data).unwrap();

        let entry = make_entry("test.dat", data, 32);
        let rs = RecoverySet {
            set_id: [0; 16],
            slice_size: 32,
            files: vec![entry],
            recovery_slice_count: 0,
        };

        let result = verify_recovery_set(&rs, dir.path());
        assert!(result.all_ok());
        assert_eq!(result.blocks_needed(), 0);
    }

    #[test]
    fn verify_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let entry = make_entry("nonexistent.dat", b"data", 32);
        let rs = RecoverySet {
            set_id: [0; 16],
            slice_size: 32,
            files: vec![entry],
            recovery_slice_count: 0,
        };

        let result = verify_recovery_set(&rs, dir.path());
        assert!(!result.all_ok());
        assert!(matches!(result.files[0].status, FileVerifyStatus::Missing));
    }

    #[test]
    fn verify_damaged_file_detects_bad_slice() {
        let dir = tempfile::tempdir().unwrap();
        let data = vec![0u8; 128];
        let entry = make_entry("test.dat", &data, 64);

        let mut damaged = data.clone();
        damaged[0] = 0xFF; // corrupt first slice
        std::fs::write(dir.path().join("test.dat"), &damaged).unwrap();

        let rs = RecoverySet {
            set_id: [0; 16],
            slice_size: 64,
            files: vec![entry],
            recovery_slice_count: 0,
        };

        let result = verify_recovery_set(&rs, dir.path());
        assert!(!result.all_ok());
        match &result.files[0].status {
            FileVerifyStatus::Damaged { bad_slices } => {
                assert!(bad_slices.contains(&0));
                assert!(!bad_slices.contains(&1)); // second slice should be fine
            }
            other => panic!("expected Damaged, got {other:?}"),
        }
    }

    #[test]
    fn verify_wrong_size_file_is_damaged() {
        let dir = tempfile::tempdir().unwrap();
        let data = vec![0u8; 128];
        let entry = make_entry("test.dat", &data, 64);

        std::fs::write(dir.path().join("test.dat"), [0u8; 100]).unwrap();

        let rs = RecoverySet {
            set_id: [0; 16],
            slice_size: 64,
            files: vec![entry],
            recovery_slice_count: 0,
        };

        let result = verify_recovery_set(&rs, dir.path());
        assert!(!result.all_ok());
        assert!(matches!(
            result.files[0].status,
            FileVerifyStatus::Damaged { .. }
        ));
    }

    #[test]
    fn verify_multiple_files_in_parallel() {
        let dir = tempfile::tempdir().unwrap();
        let mut entries = Vec::new();

        for i in 0..8 {
            let name = format!("file_{i}.dat");
            let data: Vec<u8> = (0..256).map(|b| (b as u8).wrapping_add(i)).collect();
            std::fs::write(dir.path().join(&name), &data).unwrap();
            let mut entry = make_entry(&name, &data, 64);
            entry.file_id = FileId([i; 16]);
            entries.push(entry);
        }

        let rs = RecoverySet {
            set_id: [0; 16],
            slice_size: 64,
            files: entries,
            recovery_slice_count: 0,
        };

        let result = verify_recovery_set(&rs, dir.path());
        assert!(result.all_ok());
        assert_eq!(result.files.len(), 8);
    }

    #[test]
    fn verify_mixed_ok_missing_damaged_in_parallel() {
        let dir = tempfile::tempdir().unwrap();

        // File 1: OK
        let data1 = vec![0xAA; 128];
        std::fs::write(dir.path().join("ok.dat"), &data1).unwrap();
        let mut e1 = make_entry("ok.dat", &data1, 64);
        e1.file_id = FileId([1; 16]);

        // File 2: Missing
        let data2 = vec![0xBB; 128];
        let mut e2 = make_entry("missing.dat", &data2, 64);
        e2.file_id = FileId([2; 16]);

        // File 3: Damaged
        let data3 = vec![0xCC; 128];
        let mut e3 = make_entry("damaged.dat", &data3, 64);
        e3.file_id = FileId([3; 16]);
        let mut damaged = data3.clone();
        damaged[10] = 0xFF;
        std::fs::write(dir.path().join("damaged.dat"), &damaged).unwrap();

        let rs = RecoverySet {
            set_id: [0; 16],
            slice_size: 64,
            files: vec![e1, e2, e3],
            recovery_slice_count: 0,
        };

        let result = verify_recovery_set(&rs, dir.path());
        assert!(!result.all_ok());

        let statuses: Vec<_> = result
            .files
            .iter()
            .map(|f| (&f.filename, &f.status))
            .collect();
        assert!(
            statuses
                .iter()
                .any(|(n, s)| n.as_str() == "ok.dat" && **s == FileVerifyStatus::Ok)
        );
        assert!(
            statuses
                .iter()
                .any(|(n, s)| n.as_str() == "missing.dat" && **s == FileVerifyStatus::Missing)
        );
        assert!(
            statuses.iter().any(|(n, s)| n.as_str() == "damaged.dat"
                && matches!(s, FileVerifyStatus::Damaged { .. }))
        );
    }

    #[test]
    fn verify_file_not_aligned_to_slice_size() {
        let dir = tempfile::tempdir().unwrap();
        let data = vec![0x42u8; 100]; // not a multiple of slice_size=64
        std::fs::write(dir.path().join("test.dat"), &data).unwrap();

        let entry = make_entry("test.dat", &data, 64);
        let rs = RecoverySet {
            set_id: [0; 16],
            slice_size: 64,
            files: vec![entry],
            recovery_slice_count: 0,
        };

        let result = verify_recovery_set(&rs, dir.path());
        assert!(result.all_ok());
    }
}
