use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::watch;

use bergamot_nntp::SpeedLimiter;
use bergamot_queue::{ArticleAssignment, DownloadOutcome, DownloadResult, QueueHandle};
use bergamot_yenc::YencDecoder;

use crate::cache::ArticleCache;
use crate::writer::FileWriterPool;

#[async_trait::async_trait]
pub trait ArticleFetcher: Send + Sync {
    async fn fetch_body(&self, message_id: &str, groups: &[String]) -> Result<Vec<u8>>;
}

pub struct NntpPoolFetcher {
    pool: bergamot_nntp::ServerPool,
}

impl NntpPoolFetcher {
    pub fn new(pool: bergamot_nntp::ServerPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl ArticleFetcher for NntpPoolFetcher {
    async fn fetch_body(&self, message_id: &str, groups: &[String]) -> Result<Vec<u8>> {
        self.pool
            .fetch_article(message_id, groups)
            .await
            .context("fetching article from NNTP pool")
    }
}

pub async fn download_worker(
    mut assignment_rx: tokio::sync::mpsc::Receiver<ArticleAssignment>,
    queue_handle: QueueHandle,
    fetcher: Arc<dyn ArticleFetcher>,
    inter_dir: PathBuf,
    rate_rx: watch::Receiver<u64>,
    cache: Arc<dyn ArticleCache>,
    writer_pool: Arc<FileWriterPool>,
) {
    let initial_rate = *rate_rx.borrow();
    let limiter = Arc::new(std::sync::Mutex::new(SpeedLimiter::new(initial_rate)));
    let rate_atomic = limiter.lock().unwrap().rate_ref().clone();

    let watcher_limiter = limiter.clone();
    let mut watcher_rx = rate_rx.clone();
    tokio::spawn(async move {
        while watcher_rx.changed().await.is_ok() {
            let rate = *watcher_rx.borrow();
            watcher_limiter.lock().unwrap().set_rate(rate);
        }
    });

    tracing::debug!("download worker started");

    while let Some(assignment) = assignment_rx.recv().await {
        tracing::debug!(
            message_id = %assignment.message_id,
            nzb_id = assignment.article_id.nzb_id,
            file_idx = assignment.article_id.file_idx,
            seg_idx = assignment.article_id.seg_idx,
            "received assignment"
        );
        let fetcher = fetcher.clone();
        let handle = queue_handle.clone();
        let dir = inter_dir.clone();
        let limiter = limiter.clone();
        let rate_atomic = rate_atomic.clone();
        let cache = cache.clone();
        let writer_pool = writer_pool.clone();
        tokio::spawn(async move {
            if rate_atomic.load(std::sync::atomic::Ordering::Relaxed) > 0 {
                let delay = limiter.lock().unwrap().reserve(assignment.expected_size);
                if let Some(delay) = delay {
                    tokio::time::sleep(delay).await;
                    limiter.lock().unwrap().after_sleep();
                }
            }

            let result =
                fetch_and_decode(&fetcher, &assignment, &dir, cache.as_ref(), &writer_pool).await;
            let download_result = match result {
                Ok((data, offset, crc)) => {
                    tracing::debug!(
                        message_id = %assignment.message_id,
                        offset,
                        data_len = data.len(),
                        "fetch succeeded"
                    );
                    DownloadResult {
                        article_id: assignment.article_id,
                        outcome: DownloadOutcome::Success { data, offset, crc },
                    }
                }
                Err(err) => {
                    let is_no_servers = err
                        .chain()
                        .any(|e| e.downcast_ref::<bergamot_nntp::NntpError>()
                            .is_some_and(|ne| matches!(ne, bergamot_nntp::NntpError::NoServersConfigured)));
                    if is_no_servers {
                        tracing::warn!(
                            message_id = %assignment.message_id,
                            "no servers configured, blocking download"
                        );
                        DownloadResult {
                            article_id: assignment.article_id,
                            outcome: DownloadOutcome::Blocked {
                                message: "No news servers configured".to_string(),
                            },
                        }
                    } else {
                        tracing::debug!(
                            message_id = %assignment.message_id,
                            error = %format!("{err:#}"),
                            "fetch failed"
                        );
                        DownloadResult {
                            article_id: assignment.article_id,
                            outcome: DownloadOutcome::Failure {
                                message: format!("{err:#}"),
                            },
                        }
                    }
                }
            };
            let _ = handle.report_download(download_result).await;
        });
    }

    tracing::debug!("download worker shutting down");
}

async fn fetch_and_decode(
    fetcher: &std::sync::Arc<dyn ArticleFetcher>,
    assignment: &ArticleAssignment,
    inter_dir: &Path,
    cache: &dyn ArticleCache,
    writer_pool: &FileWriterPool,
) -> Result<(Vec<u8>, u64, u32)> {
    let raw = if let Some(cached) = cache.get(&assignment.message_id) {
        tracing::debug!(message_id = %assignment.message_id, "cache hit");
        cached
    } else {
        tracing::debug!(message_id = %assignment.message_id, "cache miss");
        let fetched = fetcher
            .fetch_body(&assignment.message_id, &assignment.groups)
            .await
            .context("fetching article body")?;
        cache.put(assignment.message_id.clone(), fetched.clone());
        std::sync::Arc::new(fetched)
    };

    let mut decoder = YencDecoder::new();
    let mut segment = None;
    for line in raw.split(|&b| b == b'\n') {
        if let Some(decoded) = decoder.decode_line(line).context("decoding yenc line")? {
            segment = Some(decoded);
        }
    }

    let segment = segment.context("no yenc segment decoded")?;
    let offset = segment.begin.saturating_sub(1);

    let out_dir = inter_dir.join(format!("nzb-{}", assignment.article_id.nzb_id));
    let file_path = out_dir.join(&assignment.output_filename);
    writer_pool
        .write_segment(&file_path, offset, &segment.data)
        .await
        .context("writing segment to disk")?;

    Ok((segment.data, offset, segment.crc32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::NoopCache;
    use bergamot_queue::ArticleId;
    use std::sync::Arc;

    struct MockFetcher {
        data: Vec<u8>,
    }

    #[async_trait::async_trait]
    impl ArticleFetcher for MockFetcher {
        async fn fetch_body(&self, _message_id: &str, _groups: &[String]) -> Result<Vec<u8>> {
            Ok(self.data.clone())
        }
    }

    fn yenc_test_body() -> Vec<u8> {
        let lines: Vec<&[u8]> = vec![
            b"=ybegin line=128 size=3 name=test.bin",
            &[b'a' + 42, b'b' + 42, b'c' + 42],
            b"=yend size=3 pcrc32=352441c2",
        ];
        lines.join(&b'\n')
    }

    #[tokio::test]
    async fn fetch_and_decode_produces_correct_data() {
        let fetcher: Arc<dyn ArticleFetcher> = Arc::new(MockFetcher {
            data: yenc_test_body(),
        });
        let tmp = tempfile::tempdir().expect("tempdir");
        let assignment = ArticleAssignment {
            article_id: ArticleId {
                nzb_id: 1,
                file_idx: 0,
                seg_idx: 0,
            },
            message_id: "test@example".to_string(),
            groups: vec!["alt.test".to_string()],
            output_filename: "data.rar".to_string(),
            expected_size: 100,
        };

        let cache = NoopCache;
        let writer_pool = FileWriterPool::new();
        let (data, offset, crc) =
            fetch_and_decode(&fetcher, &assignment, tmp.path(), &cache, &writer_pool)
                .await
                .expect("decode");
        assert_eq!(data, b"abc");
        assert_eq!(offset, 0);
        assert_eq!(crc, crc32fast::hash(b"abc"));
    }

    #[tokio::test]
    async fn fetch_and_decode_writes_file() {
        let fetcher: Arc<dyn ArticleFetcher> = Arc::new(MockFetcher {
            data: yenc_test_body(),
        });
        let tmp = tempfile::tempdir().expect("tempdir");
        let assignment = ArticleAssignment {
            article_id: ArticleId {
                nzb_id: 1,
                file_idx: 0,
                seg_idx: 0,
            },
            message_id: "test@example".to_string(),
            groups: vec![],
            output_filename: "data.rar".to_string(),
            expected_size: 100,
        };

        let cache = NoopCache;
        let writer_pool = FileWriterPool::new();
        fetch_and_decode(&fetcher, &assignment, tmp.path(), &cache, &writer_pool)
            .await
            .expect("decode");
        writer_pool.flush_all().await.expect("flush");

        let file_path = tmp.path().join("nzb-1").join("data.rar");
        let content = tokio::fs::read(&file_path).await.expect("read file");
        assert_eq!(content, b"abc");
    }

    #[tokio::test]
    async fn download_worker_reports_results() {
        let fetcher: Arc<dyn ArticleFetcher> = Arc::new(MockFetcher {
            data: yenc_test_body(),
        });
        let tmp = tempfile::tempdir().expect("tempdir");
        let (_coordinator, handle, _assignment_rx, _rate_rx) = bergamot_queue::QueueCoordinator::new(
            2,
            1,
            std::path::PathBuf::from("/tmp/inter"),
            std::path::PathBuf::from("/tmp/dest"),
        );

        let assignment = ArticleAssignment {
            article_id: ArticleId {
                nzb_id: 1,
                file_idx: 0,
                seg_idx: 0,
            },
            message_id: "test@example".to_string(),
            groups: vec![],
            output_filename: "data.rar".to_string(),
            expected_size: 100,
        };

        let (tx, rx) = tokio::sync::mpsc::channel(1);
        tx.send(assignment).await.expect("send");
        drop(tx);

        let (_rate_tx, rate_rx) = tokio::sync::watch::channel(0u64);
        let cache: Arc<dyn ArticleCache> = Arc::new(NoopCache);
        let writer_pool = Arc::new(FileWriterPool::new());
        let worker_handle = tokio::spawn(download_worker(
            rx,
            handle.clone(),
            fetcher,
            tmp.path().to_path_buf(),
            rate_rx,
            cache,
            writer_pool,
        ));

        worker_handle.await.expect("worker");
    }

    #[tokio::test]
    async fn download_worker_rate_watcher_updates_limiter() {
        let (rate_tx, rate_rx) = tokio::sync::watch::channel(0u64);

        let initial_rate = *rate_rx.borrow();
        let limiter = Arc::new(std::sync::Mutex::new(bergamot_nntp::SpeedLimiter::new(
            initial_rate,
        )));

        let watcher_limiter = limiter.clone();
        let mut watcher_rx = rate_rx.clone();
        tokio::spawn(async move {
            while watcher_rx.changed().await.is_ok() {
                let rate = *watcher_rx.borrow();
                watcher_limiter.lock().unwrap().set_rate(rate);
            }
        });

        rate_tx.send(500_000).expect("send rate");
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        let locked = limiter.lock().unwrap();
        assert_eq!(locked.rate(), 500_000);
    }
}
