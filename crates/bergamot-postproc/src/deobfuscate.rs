use std::path::Path;

use bergamot_par2::RecoverySet;

const OBFUSCATED_PATTERN: &[u8] = b"0123456789abcdef";

fn looks_obfuscated(name: &str) -> bool {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    stem.len() >= 16
        && stem
            .bytes()
            .all(|b| OBFUSCATED_PATTERN.contains(&b.to_ascii_lowercase()))
}

pub async fn deobfuscate_files(
    working_dir: &Path,
    recovery_set: &RecoverySet,
) -> Vec<(String, String)> {
    let mut renames = Vec::new();

    let entries = match std::fs::read_dir(working_dir) {
        Ok(entries) => entries,
        Err(_) => return renames,
    };

    let disk_files: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
        .collect();

    for disk_name in &disk_files {
        if !looks_obfuscated(disk_name) {
            continue;
        }

        let disk_path = working_dir.join(disk_name);
        let disk_ext = Path::new(disk_name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        for par_entry in &recovery_set.files {
            if par_entry.filename == *disk_name {
                continue;
            }

            let par_ext = Path::new(&par_entry.filename)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");

            if !disk_ext.eq_ignore_ascii_case(par_ext) {
                continue;
            }

            let target_path = working_dir.join(&par_entry.filename);
            if target_path.exists() {
                continue;
            }

            let disk_len = match std::fs::metadata(&disk_path) {
                Ok(m) => m.len(),
                Err(_) => continue,
            };

            if disk_len != par_entry.length {
                continue;
            }

            let hash = match compute_file_hash_16k(&disk_path) {
                Ok(h) => h,
                Err(_) => continue,
            };

            if hash == par_entry.hash_16k.0 {
                tracing::info!(
                    from = %disk_name,
                    to = %par_entry.filename,
                    "deobfuscating file (PAR2 match)"
                );
                if std::fs::rename(&disk_path, &target_path).is_ok() {
                    renames.push((disk_name.clone(), par_entry.filename.clone()));
                }
                break;
            }
        }
    }

    renames
}

fn compute_file_hash_16k(path: &Path) -> std::io::Result<[u8; 16]> {
    use md5::Digest;
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 16384];
    let n = file.read(&mut buf)?;
    buf.truncate(n);

    let mut hasher = md5::Md5::new();
    hasher.update(&buf);
    Ok(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_looks_obfuscated() {
        assert!(looks_obfuscated("a1b2c3d4e5f67890.rar"));
        assert!(looks_obfuscated("deadbeefcafebabe12345678.nfo"));
        assert!(!looks_obfuscated("My.Movie.S01E01.mkv"));
        assert!(!looks_obfuscated("readme.txt"));
        assert!(!looks_obfuscated("abc.rar"));
    }

    #[tokio::test]
    async fn test_deobfuscate_renames_matching_file() {
        use bergamot_par2::{Md5Digest, Par2FileEntry};
        use md5::Digest;

        let dir = tempfile::tempdir().unwrap();
        let content = b"hello world data for testing deobfuscation feature!";

        let mut hasher = md5::Md5::new();
        hasher.update(content);
        let hash_16k: [u8; 16] = hasher.finalize().into();

        std::fs::write(dir.path().join("deadbeefcafebabe.rar"), content).unwrap();

        let recovery_set = RecoverySet {
            set_id: [0; 16],
            slice_size: 65536,
            files: vec![Par2FileEntry {
                file_id: bergamot_par2::FileId([0; 16]),
                filename: "My.Movie.S01E01.rar".to_string(),
                length: content.len() as u64,
                hash_full: Md5Digest(hash_16k),
                hash_16k: Md5Digest(hash_16k),
                slice_checksums: vec![],
            }],
            recovery_slice_count: 0,
            recovery_slices: vec![],
        };

        let renames = deobfuscate_files(dir.path(), &recovery_set).await;
        assert_eq!(renames.len(), 1);
        assert_eq!(renames[0].0, "deadbeefcafebabe.rar");
        assert_eq!(renames[0].1, "My.Movie.S01E01.rar");
        assert!(dir.path().join("My.Movie.S01E01.rar").exists());
        assert!(!dir.path().join("deadbeefcafebabe.rar").exists());
    }

    #[tokio::test]
    async fn test_deobfuscate_skips_non_obfuscated() {
        use bergamot_par2::{Md5Digest, Par2FileEntry};

        let dir = tempfile::tempdir().unwrap();
        let content = b"test data";
        std::fs::write(dir.path().join("My.Movie.rar"), content).unwrap();

        let recovery_set = RecoverySet {
            set_id: [0; 16],
            slice_size: 65536,
            files: vec![Par2FileEntry {
                file_id: bergamot_par2::FileId([0; 16]),
                filename: "Other.Name.rar".to_string(),
                length: content.len() as u64,
                hash_full: Md5Digest([0; 16]),
                hash_16k: Md5Digest([0; 16]),
                slice_checksums: vec![],
            }],
            recovery_slice_count: 0,
            recovery_slices: vec![],
        };

        let renames = deobfuscate_files(dir.path(), &recovery_set).await;
        assert!(renames.is_empty());
    }
}
