#[async_trait::async_trait]
pub trait FeedFetcher: Send + Sync {
    async fn fetch_url(&self, url: &str) -> Result<Vec<u8>, anyhow::Error>;
}
