use std::collections::HashMap;

use chrono::{Duration, Utc};

use crate::fetch::FeedFetcher;
use crate::rss::parse_feed;
use crate::{
    FeedConfig, FeedFilter, FeedHistoryDb, FeedInfo, FeedItemInfo, FeedItemStatus, FeedStatus,
    FilterAction,
};

pub struct FeedCoordinator {
    feeds: Vec<FeedConfig>,
    feed_infos: HashMap<u32, FeedInfo>,
    history: FeedHistoryDb,
    fetcher: Box<dyn FeedFetcher>,
    filter_cache: HashMap<String, FeedFilter>,
}

impl FeedCoordinator {
    pub fn new(
        feeds: Vec<FeedConfig>,
        history: FeedHistoryDb,
        fetcher: Box<dyn FeedFetcher>,
    ) -> Self {
        let mut feed_infos = HashMap::new();
        for feed in &feeds {
            feed_infos.insert(
                feed.id,
                FeedInfo {
                    id: feed.id,
                    name: feed.name.clone(),
                    url: feed.url.clone(),
                    last_update: None,
                    next_update: None,
                    status: FeedStatus::Idle,
                    item_count: 0,
                    error: None,
                },
            );
        }
        Self {
            feeds,
            feed_infos,
            history,
            fetcher,
            filter_cache: HashMap::new(),
        }
    }

    pub async fn process_feed(
        &mut self,
        feed: &FeedConfig,
    ) -> Result<Vec<FeedItemInfo>, anyhow::Error> {
        if let Some(info) = self.feed_infos.get_mut(&feed.id) {
            info.status = FeedStatus::Fetching;
        }

        let data = match self.fetcher.fetch_url(&feed.url).await {
            Ok(data) => data,
            Err(e) => {
                if let Some(info) = self.feed_infos.get_mut(&feed.id) {
                    info.status = FeedStatus::Error;
                    info.error = Some(e.to_string());
                }
                return Err(e);
            }
        };

        let all_items = parse_feed(&data)?;

        let filter = self.get_or_parse_filter(&feed.filter)?;

        let mut accepted = Vec::new();
        for item in all_items {
            if self.history.already_processed(feed, &item) {
                continue;
            }

            let result = filter.evaluate(&item);
            match result.action {
                FilterAction::Accept | FilterAction::Options => {
                    self.history.mark(feed, &item, FeedItemStatus::Fetched);
                    accepted.push(item);
                }
                FilterAction::Reject => {
                    self.history.mark(feed, &item, FeedItemStatus::Skipped);
                }
                FilterAction::Require | FilterAction::Comment => {}
            }
        }

        let now = Utc::now();
        if let Some(info) = self.feed_infos.get_mut(&feed.id) {
            info.status = FeedStatus::Idle;
            info.last_update = Some(now);
            info.next_update = Some(now + Duration::minutes(feed.interval_min as i64));
            info.item_count = accepted.len() as u32;
            info.error = None;
        }

        Ok(accepted)
    }

    pub async fn tick(&mut self) -> Vec<(u32, Result<Vec<FeedItemInfo>, anyhow::Error>)> {
        let now = Utc::now();
        let due_feeds: Vec<FeedConfig> = self
            .feeds
            .iter()
            .filter(|feed| {
                self.feed_infos
                    .get(&feed.id)
                    .and_then(|info| info.next_update)
                    .is_none_or(|next| now >= next)
            })
            .cloned()
            .collect();

        let mut results = Vec::new();
        for feed in &due_feeds {
            let result = self.process_feed(feed).await;
            results.push((feed.id, result));
        }
        results
    }

    fn get_or_parse_filter(&mut self, filter_text: &str) -> Result<FeedFilter, anyhow::Error> {
        if let Some(filter) = self.filter_cache.get(filter_text) {
            return Ok(filter.clone());
        }
        let filter = FeedFilter::parse(filter_text)?;
        self.filter_cache
            .insert(filter_text.to_string(), filter.clone());
        Ok(filter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::FeedFetcher;

    struct MockFetcher {
        response: Vec<u8>,
    }

    #[async_trait::async_trait]
    impl FeedFetcher for MockFetcher {
        async fn fetch_url(&self, _url: &str) -> Result<Vec<u8>, anyhow::Error> {
            Ok(self.response.clone())
        }
    }

    const TEST_RSS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <item>
      <title>Good.Show.S01E01.720p</title>
      <link>https://example.com/1.nzb</link>
    </item>
    <item>
      <title>Bad.Show.S01E01.720p</title>
      <link>https://example.com/2.nzb</link>
    </item>
    <item>
      <title>Good.Show.S01E02.720p</title>
      <link>https://example.com/3.nzb</link>
    </item>
  </channel>
</rss>"#;

    fn test_feed_config() -> FeedConfig {
        FeedConfig {
            id: 1,
            name: "Test Feed".to_string(),
            url: "https://example.com/rss".to_string(),
            filter: "A: title(Good.*)".to_string(),
            interval_min: 15,
            backlog: false,
            pause_nzb: false,
            category: "TV".to_string(),
            priority: 0,
            extensions: vec![],
        }
    }

    #[tokio::test]
    async fn accepted_items_returned() {
        let fetcher = MockFetcher {
            response: TEST_RSS.as_bytes().to_vec(),
        };
        let feed = test_feed_config();
        let history = FeedHistoryDb::new(30);
        let mut coordinator = FeedCoordinator::new(vec![feed.clone()], history, Box::new(fetcher));

        let items = coordinator.process_feed(&feed).await.unwrap();
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| i.title.starts_with("Good.")));
    }

    #[tokio::test]
    async fn rejected_items_filtered() {
        let fetcher = MockFetcher {
            response: TEST_RSS.as_bytes().to_vec(),
        };
        let feed = test_feed_config();
        let history = FeedHistoryDb::new(30);
        let mut coordinator = FeedCoordinator::new(vec![feed.clone()], history, Box::new(fetcher));

        let items = coordinator.process_feed(&feed).await.unwrap();
        assert!(!items.iter().any(|i| i.title.starts_with("Bad.")));
    }

    #[tokio::test]
    async fn duplicates_skipped_via_history() {
        let fetcher = MockFetcher {
            response: TEST_RSS.as_bytes().to_vec(),
        };
        let feed = test_feed_config();
        let history = FeedHistoryDb::new(30);
        let mut coordinator = FeedCoordinator::new(vec![feed.clone()], history, Box::new(fetcher));

        let first = coordinator.process_feed(&feed).await.unwrap();
        assert_eq!(first.len(), 2);

        let second = coordinator.process_feed(&feed).await.unwrap();
        assert_eq!(second.len(), 0);
    }

    #[tokio::test]
    async fn tick_processes_due_feeds() {
        let fetcher = MockFetcher {
            response: TEST_RSS.as_bytes().to_vec(),
        };
        let feed = test_feed_config();
        let history = FeedHistoryDb::new(30);
        let mut coordinator = FeedCoordinator::new(vec![feed.clone()], history, Box::new(fetcher));

        let results = coordinator.tick().await;
        assert_eq!(results.len(), 1);
        let (id, result) = &results[0];
        assert_eq!(*id, 1);
        assert_eq!(result.as_ref().unwrap().len(), 2);
    }
}
