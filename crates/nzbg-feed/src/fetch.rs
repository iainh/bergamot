#[async_trait::async_trait]
pub trait FeedFetcher: Send + Sync {
    async fn fetch_url(&self, url: &str) -> Result<Vec<u8>, anyhow::Error>;
}

pub struct HttpFeedFetcher;

#[async_trait::async_trait]
impl FeedFetcher for HttpFeedFetcher {
    async fn fetch_url(&self, url: &str) -> Result<Vec<u8>, anyhow::Error> {
        let resp = reqwest::get(url).await?;
        if !resp.status().is_success() {
            anyhow::bail!("HTTP {} for {url}", resp.status());
        }
        let bytes = resp.bytes().await?;
        Ok(bytes.to_vec())
    }
}
