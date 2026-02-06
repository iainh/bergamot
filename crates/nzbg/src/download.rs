use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use nzbg_queue::{ArticleAssignment, DownloadOutcome, DownloadResult, QueueHandle};
use nzbg_yenc::YencDecoder;

#[async_trait::async_trait]
pub trait ArticleFetcher: Send + Sync {
    async fn fetch_body(&self, message_id: &str) -> Result<Vec<Vec<u8>>>;
}

pub async fn download_worker(
    mut assignment_rx: tokio::sync::mpsc::Receiver<ArticleAssignment>,
    queue_handle: QueueHandle,
    fetcher: std::sync::Arc<dyn ArticleFetcher>,
    inter_dir: PathBuf,
) {
    while let Some(assignment) = assignment_rx.recv().await {
        let fetcher = fetcher.clone();
        let handle = queue_handle.clone();
        let dir = inter_dir.clone();
        tokio::spawn(async move {
            let result = fetch_and_decode(&fetcher, &assignment, &dir).await;
            let download_result = match result {
                Ok((data, offset, crc)) => DownloadResult {
                    article_id: assignment.article_id,
                    outcome: DownloadOutcome::Success { data, offset, crc },
                },
                Err(err) => DownloadResult {
                    article_id: assignment.article_id,
                    outcome: DownloadOutcome::Failure {
                        message: format!("{err:#}"),
                    },
                },
            };
            let _ = handle.report_download(download_result).await;
        });
    }
}

async fn fetch_and_decode(
    fetcher: &std::sync::Arc<dyn ArticleFetcher>,
    assignment: &ArticleAssignment,
    inter_dir: &Path,
) -> Result<(Vec<u8>, u64, u32)> {
    let lines = fetcher
        .fetch_body(&assignment.message_id)
        .await
        .context("fetching article body")?;

    let mut decoder = YencDecoder::new();
    let mut segment = None;
    for line in &lines {
        if let Some(decoded) = decoder.decode_line(line).context("decoding yenc line")? {
            segment = Some(decoded);
        }
    }

    let segment = segment.context("no yenc segment decoded")?;
    let offset = segment.begin.saturating_sub(1);

    let out_dir = inter_dir.join(format!("nzb-{}", assignment.article_id.nzb_id));
    tokio::fs::create_dir_all(&out_dir)
        .await
        .context("creating output directory")?;

    let file_path = out_dir.join(format!("file-{}", assignment.article_id.file_idx));
    write_segment(&file_path, offset, &segment.data)
        .await
        .context("writing segment to disk")?;

    Ok((segment.data, offset, segment.crc32))
}

async fn write_segment(path: &Path, offset: u64, data: &[u8]) -> Result<()> {
    use tokio::io::AsyncWriteExt;

    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .await
        .context("opening output file")?;

    let std_file = file.into_std().await;
    let target_len = offset + data.len() as u64;
    if std_file.metadata()?.len() < target_len {
        std_file.set_len(target_len)?;
    }

    let mut file = tokio::fs::File::from_std(std_file);
    use tokio::io::AsyncSeekExt;
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    file.write_all(data).await?;
    file.flush().await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nzbg_queue::ArticleId;
    use std::sync::Arc;

    struct MockFetcher {
        lines: Vec<Vec<u8>>,
    }

    #[async_trait::async_trait]
    impl ArticleFetcher for MockFetcher {
        async fn fetch_body(&self, _message_id: &str) -> Result<Vec<Vec<u8>>> {
            Ok(self.lines.clone())
        }
    }

    fn yenc_test_lines() -> Vec<Vec<u8>> {
        vec![
            b"=ybegin line=128 size=3 name=test.bin".to_vec(),
            vec![b'a' + 42, b'b' + 42, b'c' + 42],
            b"=yend size=3 pcrc32=352441c2".to_vec(),
        ]
    }

    #[tokio::test]
    async fn fetch_and_decode_produces_correct_data() {
        let fetcher: Arc<dyn ArticleFetcher> = Arc::new(MockFetcher {
            lines: yenc_test_lines(),
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
        };

        let (data, offset, crc) = fetch_and_decode(&fetcher, &assignment, tmp.path())
            .await
            .expect("decode");
        assert_eq!(data, b"abc");
        assert_eq!(offset, 0);
        assert_eq!(crc, crc32fast::hash(b"abc"));
    }

    #[tokio::test]
    async fn fetch_and_decode_writes_file() {
        let fetcher: Arc<dyn ArticleFetcher> = Arc::new(MockFetcher {
            lines: yenc_test_lines(),
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
        };

        fetch_and_decode(&fetcher, &assignment, tmp.path())
            .await
            .expect("decode");

        let file_path = tmp.path().join("nzb-1").join("file-0");
        let content = tokio::fs::read(&file_path).await.expect("read file");
        assert_eq!(content, b"abc");
    }

    #[tokio::test]
    async fn download_worker_reports_results() {
        let fetcher: Arc<dyn ArticleFetcher> = Arc::new(MockFetcher {
            lines: yenc_test_lines(),
        });
        let tmp = tempfile::tempdir().expect("tempdir");
        let (_coordinator, handle, _assignment_rx) = nzbg_queue::QueueCoordinator::new(2, 1);

        let assignment = ArticleAssignment {
            article_id: ArticleId {
                nzb_id: 1,
                file_idx: 0,
                seg_idx: 0,
            },
            message_id: "test@example".to_string(),
            groups: vec![],
        };

        let (tx, rx) = tokio::sync::mpsc::channel(1);
        tx.send(assignment).await.expect("send");
        drop(tx);

        let worker_handle = tokio::spawn(download_worker(
            rx,
            handle.clone(),
            fetcher,
            tmp.path().to_path_buf(),
        ));

        worker_handle.await.expect("worker");
    }
}
