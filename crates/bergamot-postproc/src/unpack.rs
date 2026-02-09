use std::path::Path;

use async_trait::async_trait;
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnpackResult {
    Success,
    Failure(String),
    Password,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveType {
    Rar,
    SevenZip,
    Zip,
}

#[async_trait]
pub trait Unpacker: Send + Sync {
    async fn unpack(&self, archive: &Path, working_dir: &Path) -> UnpackResult;
}

pub fn detect_archives(working_dir: &Path) -> Vec<(ArchiveType, std::path::PathBuf)> {
    let entries = match std::fs::read_dir(working_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut archives = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase());
        match ext.as_deref() {
            Some("rar") => archives.push((ArchiveType::Rar, path)),
            Some("7z") => archives.push((ArchiveType::SevenZip, path)),
            Some("zip") => archives.push((ArchiveType::Zip, path)),
            _ => {}
        }
    }
    archives.sort_by(|a, b| a.1.cmp(&b.1));
    archives
}

#[derive(Debug, Clone)]
pub struct CommandLineUnpacker;

#[async_trait]
impl Unpacker for CommandLineUnpacker {
    async fn unpack(&self, archive: &Path, working_dir: &Path) -> UnpackResult {
        let ext = archive
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase());

        let result = match ext.as_deref() {
            Some("rar") => {
                Command::new("unrar")
                    .args(["x", "-y", "-o+"])
                    .arg(archive)
                    .current_dir(working_dir)
                    .output()
                    .await
            }
            Some("7z") | Some("zip") => {
                Command::new("7z")
                    .args(["x", "-y", "-o"])
                    .arg(archive)
                    .current_dir(working_dir)
                    .output()
                    .await
            }
            _ => {
                return UnpackResult::Failure(format!(
                    "unsupported archive: {}",
                    archive.display()
                ));
            }
        };

        match result {
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if output.status.success() {
                    UnpackResult::Success
                } else if stderr.contains("password") || stderr.contains("encrypted") {
                    UnpackResult::Password
                } else {
                    UnpackResult::Failure(stderr.into_owned())
                }
            }
            Err(e) => UnpackResult::Failure(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_archives_finds_rar() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file.rar"), b"").unwrap();
        std::fs::write(dir.path().join("file.txt"), b"").unwrap();
        let archives = detect_archives(dir.path());
        assert_eq!(archives.len(), 1);
        assert_eq!(archives[0].0, ArchiveType::Rar);
    }

    #[test]
    fn detect_archives_finds_multiple_types() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rar"), b"").unwrap();
        std::fs::write(dir.path().join("b.7z"), b"").unwrap();
        std::fs::write(dir.path().join("c.zip"), b"").unwrap();
        let archives = detect_archives(dir.path());
        assert_eq!(archives.len(), 3);
    }

    #[test]
    fn detect_archives_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let archives = detect_archives(dir.path());
        assert!(archives.is_empty());
    }
}
