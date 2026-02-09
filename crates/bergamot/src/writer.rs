use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use dashmap::DashMap;
use tokio::sync::{mpsc, oneshot};

struct WriteRequest {
    offset: u64,
    data: Vec<u8>,
    reply: oneshot::Sender<Result<()>>,
}

pub struct FileWriterPool {
    writers: Arc<DashMap<PathBuf, mpsc::Sender<WriteRequest>>>,
}

impl Default for FileWriterPool {
    fn default() -> Self {
        Self::new()
    }
}

impl FileWriterPool {
    pub fn new() -> Self {
        Self {
            writers: Arc::new(DashMap::new()),
        }
    }

    pub async fn write_segment(&self, path: &Path, offset: u64, data: &[u8]) -> Result<()> {
        let tx = self.get_or_create(path).await?;
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(WriteRequest {
            offset,
            data: data.to_vec(),
            reply: reply_tx,
        })
        .await
        .map_err(|_| anyhow::anyhow!("writer task closed"))?;
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("writer task dropped reply"))?
    }

    async fn get_or_create(&self, path: &Path) -> Result<mpsc::Sender<WriteRequest>> {
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

        let (tx, rx) = mpsc::channel::<WriteRequest>(64);
        tokio::spawn(writer_task(file, rx));

        self.writers.entry(path.to_path_buf()).or_insert(tx.clone());
        Ok(tx)
    }

    pub async fn flush_all(&self) -> Result<()> {
        let keys: Vec<PathBuf> = self.writers.iter().map(|e| e.key().clone()).collect();
        for key in keys {
            if let Some((_, tx)) = self.writers.remove(&key) {
                drop(tx);
            }
        }
        tokio::task::yield_now().await;
        Ok(())
    }
}

async fn writer_task(mut file: tokio::fs::File, mut rx: mpsc::Receiver<WriteRequest>) {
    use tokio::io::{AsyncSeekExt, AsyncWriteExt};

    let mut allocated_len = file.metadata().await.map(|m| m.len()).unwrap_or(0);

    while let Some(req) = rx.recv().await {
        let result = async {
            let target_len = req.offset + req.data.len() as u64;
            if allocated_len < target_len {
                file.set_len(target_len).await?;
                allocated_len = target_len;
            }
            file.seek(std::io::SeekFrom::Start(req.offset)).await?;
            file.write_all(&req.data).await?;
            Ok(())
        }
        .await;
        let _ = req.reply.send(result);
    }

    let _ = tokio::io::AsyncWriteExt::flush(&mut file).await;
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

    #[tokio::test]
    async fn writer_task_flushes_on_drop() {
        let pool = FileWriterPool::new();
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("drop.bin");

        pool.write_segment(&path, 0, b"flushed")
            .await
            .expect("write");
        pool.flush_all().await.expect("flush");

        let content = tokio::fs::read(&path).await.expect("read");
        assert_eq!(content, b"flushed");
    }
}
