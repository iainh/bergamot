use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use dashmap::DashMap;
use tokio::sync::Mutex;

struct FileEntry {
    file: Mutex<tokio::fs::File>,
    allocated_len: std::sync::atomic::AtomicU64,
}

pub struct FileWriterPool {
    writers: Arc<DashMap<PathBuf, Arc<FileEntry>>>,
}

impl FileWriterPool {
    pub fn new() -> Self {
        Self {
            writers: Arc::new(DashMap::new()),
        }
    }

    pub async fn write_segment(&self, path: &Path, offset: u64, data: &[u8]) -> Result<()> {
        use tokio::io::{AsyncSeekExt, AsyncWriteExt};

        let entry = self.get_or_create(path).await?;
        let target_len = offset + data.len() as u64;

        let current = entry
            .allocated_len
            .load(std::sync::atomic::Ordering::Acquire);
        if current < target_len {
            entry
                .allocated_len
                .fetch_max(target_len, std::sync::atomic::Ordering::AcqRel);
            let file_guard = entry.file.lock().await;
            file_guard.set_len(target_len).await?;
        }

        let mut file_guard = entry.file.lock().await;
        file_guard.seek(std::io::SeekFrom::Start(offset)).await?;
        file_guard.write_all(data).await?;
        Ok(())
    }

    async fn get_or_create(&self, path: &Path) -> Result<Arc<FileEntry>> {
        if let Some(entry) = self.writers.get(path) {
            return Ok(entry.value().clone());
        }

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .context("creating output directory")?;
        }

        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .read(true)
            .open(path)
            .await
            .context("opening output file")?;

        let metadata = file.metadata().await.context("reading file metadata")?;
        let entry = Arc::new(FileEntry {
            file: Mutex::new(file),
            allocated_len: std::sync::atomic::AtomicU64::new(metadata.len()),
        });
        self.writers
            .entry(path.to_path_buf())
            .or_insert(entry.clone());
        Ok(entry)
    }

    pub async fn flush_all(&self) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        for entry in self.writers.iter() {
            let mut f = entry.value().file.lock().await;
            f.flush().await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn writer_pool_creates_file_and_writes() {
        let pool = FileWriterPool::new();
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("test.bin");

        pool.write_segment(&path, 0, b"hello").await.expect("write");
        pool.flush_all().await.expect("flush");

        let content = tokio::fs::read(&path).await.expect("read");
        assert_eq!(content, b"hello");
    }

    #[tokio::test]
    async fn writer_pool_reuses_open_file() {
        let pool = FileWriterPool::new();
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("test.bin");

        pool.write_segment(&path, 0, b"abc").await.expect("write1");
        pool.write_segment(&path, 3, b"def").await.expect("write2");
        pool.flush_all().await.expect("flush");

        let content = tokio::fs::read(&path).await.expect("read");
        assert_eq!(content, b"abcdef");
    }

    #[tokio::test]
    async fn writer_pool_writes_at_offset() {
        let pool = FileWriterPool::new();
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("test.bin");

        pool.write_segment(&path, 5, b"world").await.expect("write");
        pool.flush_all().await.expect("flush");

        let content = tokio::fs::read(&path).await.expect("read");
        assert_eq!(content.len(), 10);
        assert_eq!(&content[5..], b"world");
    }

    #[tokio::test]
    async fn writer_pool_flush_all() {
        let pool = FileWriterPool::new();
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("flush.bin");

        pool.write_segment(&path, 0, b"data").await.expect("write");
        pool.flush_all().await.expect("flush");

        let content = tokio::fs::read(&path).await.expect("read");
        assert_eq!(content, b"data");
    }

    #[tokio::test]
    async fn writer_pool_tracks_allocated_length() {
        let pool = FileWriterPool::new();
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("alloc.bin");

        pool.write_segment(&path, 0, b"abc").await.expect("write1");
        pool.write_segment(&path, 10, b"xyz").await.expect("write2");
        pool.flush_all().await.expect("flush");

        let content = tokio::fs::read(&path).await.expect("read");
        assert_eq!(content.len(), 13);
        assert_eq!(&content[0..3], b"abc");
        assert_eq!(&content[10..13], b"xyz");
    }
}
