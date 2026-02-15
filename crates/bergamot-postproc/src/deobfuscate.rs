use std::path::Path;

use bergamot_par2::RecoverySet;

fn looks_obfuscated(name: &str) -> bool {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    if stem.len() < 16 {
        return false;
    }
    // Pure hex string (common obfuscation)
    let is_hex = stem
        .bytes()
        .all(|b| b.is_ascii_hexdigit());
    // Pure alphanumeric with no separators (e.g., base62/base64-style obfuscation)
    let is_alnum_no_sep = stem.bytes().all(|b| b.is_ascii_alphanumeric())
        && !stem.contains('.')
        && !stem.contains('-')
        && !stem.contains('_')
        && !stem.contains(' ');
    is_hex || is_alnum_no_sep
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

/// Fallback deobfuscation when no PAR2 data is available.
/// If there is exactly one obfuscated file, rename it using the NZB name.
pub fn deobfuscate_by_nzb_name(working_dir: &Path, nzb_name: &str) -> Option<(String, String)> {
    let entries: Vec<String> = match std::fs::read_dir(working_dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
            .collect(),
        Err(_) => return None,
    };

    let obfuscated: Vec<&String> = entries.iter().filter(|n| looks_obfuscated(n)).collect();

    if obfuscated.len() != 1 {
        return None;
    }

    let old_name = obfuscated[0];
    let ext = Path::new(old_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    let base_name = nzb_name
        .strip_suffix(".nzb")
        .or_else(|| nzb_name.strip_suffix(".NZB"))
        .unwrap_or(nzb_name);

    let new_name = if ext.is_empty() {
        base_name.to_string()
    } else {
        format!("{base_name}.{ext}")
    };

    if new_name == *old_name {
        return None;
    }

    let old_path = working_dir.join(old_name);
    let new_path = working_dir.join(&new_name);

    if new_path.exists() {
        return None;
    }

    tracing::info!(
        from = %old_name,
        to = %new_name,
        "deobfuscating file (NZB name fallback)"
    );

    if std::fs::rename(&old_path, &new_path).is_ok() {
        Some((old_name.clone(), new_name))
    } else {
        None
    }
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
        // Hex-style obfuscation
        assert!(looks_obfuscated("a1b2c3d4e5f67890.rar"));
        assert!(looks_obfuscated("deadbeefcafebabe12345678.nfo"));
        // Base62/alphanumeric obfuscation
        assert!(looks_obfuscated("cpbsfRk7RFtu5ghi4nmTFDem19hjfPL6.mkv"));
        assert!(looks_obfuscated("AbCdEfGhIjKlMnOpQrSt.rar"));
        // Normal filenames
        assert!(!looks_obfuscated("My.Movie.S01E01.mkv"));
        assert!(!looks_obfuscated("readme.txt"));
        assert!(!looks_obfuscated("abc.rar"));
        // Filenames with separators should not be flagged
        assert!(!looks_obfuscated("Cool.Show.S02E03.mkv"));
        assert!(!looks_obfuscated("some-long-file-name-here.mkv"));
        assert!(!looks_obfuscated("some_long_file_name_here.mkv"));
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

    #[test]
    fn test_deobfuscate_by_nzb_name_renames_single_obfuscated_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("cpbsfRk7RFtu5ghi4nmTFDem19hjfPL6.mkv"),
            b"video data",
        )
        .unwrap();

        let result = deobfuscate_by_nzb_name(
            dir.path(),
            "My.Show.S02E05.Episode.Title.1080p.WEB-DL.DDP5.1.H.264-GRP",
        );
        assert!(result.is_some());
        let (from, to) = result.unwrap();
        assert_eq!(from, "cpbsfRk7RFtu5ghi4nmTFDem19hjfPL6.mkv");
        assert_eq!(to, "My.Show.S02E05.Episode.Title.1080p.WEB-DL.DDP5.1.H.264-GRP.mkv");
        assert!(dir.path().join(&to).exists());
        assert!(!dir.path().join(&from).exists());
    }

    #[test]
    fn test_deobfuscate_by_nzb_name_strips_nzb_extension() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("cpbsfRk7RFtu5ghi4nmTFDem19hjfPL6.mkv"),
            b"video data",
        )
        .unwrap();

        let result = deobfuscate_by_nzb_name(
            dir.path(),
            "My.Show.S02E05.Episode.Title.1080p.WEB-DL.DDP5.1.H.264-GRP.nzb",
        );
        assert!(result.is_some());
        let (from, to) = result.unwrap();
        assert_eq!(from, "cpbsfRk7RFtu5ghi4nmTFDem19hjfPL6.mkv");
        assert_eq!(to, "My.Show.S02E05.Episode.Title.1080p.WEB-DL.DDP5.1.H.264-GRP.mkv");
        assert!(dir.path().join(&to).exists());
    }

    #[test]
    fn test_deobfuscate_by_nzb_name_skips_when_multiple_obfuscated() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("cpbsfRk7RFtu5ghi4nmTFDem19hjfPL6.mkv"),
            b"video",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("xYzAbCdEfGhIjKlMnOpQrStUv.srt"),
            b"subs",
        )
        .unwrap();

        let result = deobfuscate_by_nzb_name(dir.path(), "My.Show.S01E01");
        assert!(result.is_none());
    }

    #[test]
    fn test_deobfuscate_by_nzb_name_skips_non_obfuscated() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("My.Movie.S01E01.mkv"), b"video").unwrap();

        let result = deobfuscate_by_nzb_name(dir.path(), "My.Movie.S01E01");
        assert!(result.is_none());
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
