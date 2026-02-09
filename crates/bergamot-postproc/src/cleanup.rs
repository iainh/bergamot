use std::path::Path;

use crate::error::PostProcessError;

pub async fn cleanup_archives(
    working_dir: &Path,
) -> Result<Vec<std::path::PathBuf>, PostProcessError> {
    let mut removed = Vec::new();
    let mut entries = tokio::fs::read_dir(working_dir).await?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if should_remove(&path) {
            tokio::fs::remove_file(&path).await?;
            removed.push(path);
        }
    }

    removed.sort();
    Ok(removed)
}

fn should_remove(path: &Path) -> bool {
    let name = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n.to_lowercase(),
        None => return false,
    };

    if name.ends_with(".par2") {
        return true;
    }

    if name.contains(".vol") && name.ends_with(".par2") {
        return true;
    }

    let ext = match path.extension().and_then(|e| e.to_str()) {
        Some(e) => e.to_lowercase(),
        None => return false,
    };

    if ext == "rar" {
        return true;
    }

    if ext.starts_with('r')
        && ext.len() == 3
        && let Ok(n) = ext[1..].parse::<u32>()
        && n <= 99
    {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_remove_rar_files() {
        assert!(should_remove(Path::new("file.rar")));
        assert!(should_remove(Path::new("file.r00")));
        assert!(should_remove(Path::new("file.r99")));
    }

    #[test]
    fn should_remove_par2_files() {
        assert!(should_remove(Path::new("file.par2")));
        assert!(should_remove(Path::new("file.vol01+02.par2")));
    }

    #[test]
    fn should_not_remove_regular_files() {
        assert!(!should_remove(Path::new("file.mkv")));
        assert!(!should_remove(Path::new("file.nfo")));
        assert!(!should_remove(Path::new("file.txt")));
    }

    #[tokio::test]
    async fn cleanup_removes_expected_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file.rar"), b"data").unwrap();
        std::fs::write(dir.path().join("file.r00"), b"data").unwrap();
        std::fs::write(dir.path().join("file.par2"), b"data").unwrap();
        std::fs::write(dir.path().join("file.vol01+02.par2"), b"data").unwrap();
        std::fs::write(dir.path().join("movie.mkv"), b"data").unwrap();

        let removed = cleanup_archives(dir.path()).await.unwrap();
        assert_eq!(removed.len(), 4);

        assert!(dir.path().join("movie.mkv").exists());
        assert!(!dir.path().join("file.rar").exists());
        assert!(!dir.path().join("file.r00").exists());
        assert!(!dir.path().join("file.par2").exists());
        assert!(!dir.path().join("file.vol01+02.par2").exists());
    }
}
