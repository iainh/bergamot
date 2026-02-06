use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::Mutex;

pub struct FileWriterPool {
    writers: Arc<Mutex<HashMap<PathBuf, Arc<Mutex<tokio::fs::File>>>>>,
}

impl FileWriterPool {
    pub fn new() -> Self {
        Self {
            writers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn write_segment(&self, path: &Path, offset: u64, data: &[u8]) -> Result<()> {
        use tokio::io::{AsyncSeekExt, AsyncWriteExt};

        let file = self.get_or_create(path).await?;
        let mut file_guard = file.lock().await;

        let std_file = file_guard
            .try_clone()
            .await
            .context("cloning file handle")?;
        let std_file = std_file.into_std().await;
        let target_len = offset + data.len() as u64;
        if std_file.metadata()?.len() < target_len {
            std_file.set_len(target_len)?;
        }
        drop(std_file);

        file_guard.seek(std::io::SeekFrom::Start(offset)).await?;
        file_guard.write_all(data).await?;
        Ok(())
    }

    async fn get_or_create(&self, path: &Path) -> Result<Arc<Mutex<tokio::fs::File>>> {
        let mut writers = self.writers.lock().await;
        if let Some(writer) = writers.get(path) {
            return Ok(writer.clone());
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

        let writer = Arc::new(Mutex::new(file));
        writers.insert(path.to_path_buf(), writer.clone());
        Ok(writer)
    }

    pub async fn flush_all(&self) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        let writers = self.writers.lock().await;
        for file in writers.values() {
            let mut f = file.lock().await;
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
}
