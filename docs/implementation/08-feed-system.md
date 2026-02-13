# Feed System

The feed system automatically discovers and downloads NZBs from RSS/Atom feeds. It periodically polls configured feeds, applies filter rules to incoming items, and enqueues matching NZBs into the download queue.

## Feed Configuration

Each feed is defined in the configuration file with a numeric suffix (e.g., `Feed1.Name`, `Feed1.URL`):

| Option | Type | Description |
|--------|------|-------------|
| `Name` | `String` | Display name for the feed |
| `URL` | `String` | RSS/Atom feed URL |
| `Filter` | `String` | Multi-line filter expression (see below) |
| `Interval` | `u32` | Poll interval in minutes (default: 15) |
| `Backlog` | `bool` | Process old items on first fetch |
| `PauseNzb` | `bool` | Add NZBs in paused state |
| `Category` | `String` | Default category for matched items |
| `Priority` | `i32` | Priority assigned to matched NZBs |
| `Extensions` | `Vec<String>` | Post-processing extensions for matched NZBs |

```rust
/// Configuration for a single RSS/Atom feed.
pub struct FeedConfig {
    pub id: u32,
    pub name: String,
    pub url: String,
    pub filter: String,
    pub interval_min: u32,
    pub backlog: bool,
    pub pause_nzb: bool,
    pub category: String,
    pub priority: i32,
    pub extensions: Vec<String>,
}
```

## Feed Parsing

The feed parser supports both **RSS 2.0** and **Atom** formats. Parsing uses `quick-xml` to stream through the XML without loading the entire document into memory.

### RSS 2.0

Items are extracted from `<channel><item>` elements:

```xml
<rss version="2.0" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">
  <channel>
    <item>
      <title>Example.NZB</title>
      <link>https://indexer.example/nzb/12345</link>
      <category>TV > HD</category>
      <enclosure url="https://indexer.example/getnzb/12345" length="524288000" type="application/x-nzb"/>
      <newznab:attr name="size" value="524288000"/>
      <pubDate>Sun, 01 Jan 2026 12:00:00 +0000</pubDate>
    </item>
  </channel>
</rss>
```

### Atom

Items are extracted from `<feed><entry>` elements:

```xml
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <title>Example.NZB</title>
    <link href="https://indexer.example/nzb/12345"/>
    <category term="TV"/>
    <updated>2026-01-01T12:00:00Z</updated>
  </entry>
</feed>
```

### Extracted Fields

From each item the parser produces a `FeedItemInfo`:

```rust
pub struct FeedItemInfo {
    pub title: String,
    pub url: String,
    pub filename: String,
    pub category: String,
    pub size: u64,
    pub time: chrono::DateTime<chrono::Utc>,
    pub description: String,
    pub season: Option<String>,
    pub episode: Option<String>,
    pub imdb_id: Option<String>,
    pub rageid: Option<u32>,
    pub tvdbid: Option<u32>,
    pub tvmazeid: Option<u32>,
    pub dupe_key: String,
    pub dupe_score: i32,
    pub dupe_mode: DupeMode,
    pub status: FeedItemStatus,
    pub added_nzb_id: Option<u32>,
}

pub enum FeedItemStatus {
    Unknown,
    Backlog,
    Fetched,
    Skipped,
}
```

The NZB URL is resolved from (in priority order):
1. `<enclosure url="..."/>` with type `application/x-nzb`
2. `<link>` element containing `/getnzb/` or ending in `.nzb`
3. Direct `<link>` as fallback

## Feed Filter Language

Filters are multi-line rules evaluated top-to-bottom against each feed item. The first matching rule determines the action.

### Rule Syntax

```
Action: Field1(pattern1) Field2(pattern2) ...
```

### Actions

| Action | Meaning |
|--------|---------|
| `A` or `Accept` | Accept the item for download |
| `R` or `Reject` | Reject (skip) the item |
| `Q` or `Require` | Require — reject all items that do NOT match |
| `O` or `Options` | Set options (category, priority, pause, dupe key) without accepting/rejecting |
| `C` or `Comment` | Comment line, ignored |

### Fields

| Field | Example | Description |
|-------|---------|-------------|
| `title` | `title(.*720p.*)` | Match against item title (regex) |
| `category` | `category(TV*)` | Match against category (wildcard) |
| `size` | `size(<500MB)` | File size comparison (`<`, `>`, `<=`, `>=`) |
| `age` | `age(<2d)` | Item age comparison |
| `imdb` | `imdb(tt1234567)` | IMDB ID match |
| `season` | `season(3)` | Season number |
| `episode` | `episode(1-5)` | Episode number or range |
| `rating` | `rating(>7.0)` | Rating threshold |
| `genre` | `genre(*Action*)` | Genre match |
| `tag` | `tag(*HD*)` | Tag match |
| `priority` | `priority(>0)` | Priority override |
| `pause` | `pause(yes)` | Override pause state |
| `category:` | `category:(Movies)` | Override category assignment |
| `dupekey` | `dupekey(show-123)` | Set duplicate key |
| `dupescore` | `dupescore(500)` | Set duplicate score |

### Regex Support

Title and category fields support POSIX-like regex (mapped to the `regex` crate in Rust):

```
A: title(.*\b720p\b.*HDTV.*)
R: title(.*\bCAM\b.*)
```

### Filter Evaluation

```rust
pub struct FeedFilter {
    pub rules: Vec<FilterRule>,
}

pub struct FilterRule {
    pub action: FilterAction,
    pub conditions: Vec<FilterCondition>,
}

pub enum FilterAction {
    Accept,
    Reject,
    Require,
    Options,
    Comment,
}

pub struct FilterCondition {
    pub field: FilterField,
    pub pattern: String,
    pub is_regex: bool,
}

impl FeedFilter {
    /// Parse a multi-line filter string into structured rules.
    pub fn parse(filter_text: &str) -> Result<Self, FilterParseError> {
        // ...
        todo!()
    }

    /// Evaluate the filter against a feed item. Returns the action
    /// to take and any option overrides.
    pub fn evaluate(&self, item: &FeedItemInfo) -> FilterResult {
        for rule in &self.rules {
            if rule.matches(item) {
                return FilterResult {
                    action: rule.action.clone(),
                    // option overrides extracted from the rule
                    ..Default::default()
                };
            }
        }
        FilterResult::default() // no match → reject by default
    }
}
```

### Duplicate Key Generation

When a filter sets `dupekey`, or the item has IMDB/TVDB/RAGEID metadata, a
duplicate key is generated so the queue's duplicate checker can prevent
re-downloads:

```
dupekey = "imdb-tt1234567" | "tvdb-12345-S03E05" | custom string
```

## FeedCoordinator

The `FeedCoordinator` runs as a long-lived async task that manages all feeds:

```
┌─────────────────────────────────────────────────────┐
│                  FeedCoordinator                    │
│                                                     │
│  ┌───────────┐   ┌───────────┐   ┌───────────┐    │
│  │  Feed #1  │   │  Feed #2  │   │  Feed #3  │    │
│  │ interval  │   │ interval  │   │ interval  │    │
│  │  15 min   │   │  30 min   │   │  60 min   │    │
│  └─────┬─────┘   └─────┬─────┘   └─────┬─────┘    │
│        │               │               │           │
│        ▼               ▼               ▼           │
│  ┌─────────────────────────────────────────────┐   │
│  │          HTTP Fetch + XML Parse              │   │
│  └──────────────────┬──────────────────────────┘   │
│                     │                               │
│                     ▼                               │
│  ┌─────────────────────────────────────────────┐   │
│  │            Filter Evaluation                 │   │
│  └──────────────────┬──────────────────────────┘   │
│                     │                               │
│                     ▼                               │
│  ┌─────────────────────────────────────────────┐   │
│  │  Duplicate Check + Feed History Lookup       │   │
│  └──────────────────┬──────────────────────────┘   │
│                     │                               │
│                     ▼                               │
│  ┌─────────────────────────────────────────────┐   │
│  │   Enqueue Accepted NZBs → QueueCoordinator   │   │
│  └─────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────┘
```

```rust
pub struct FeedCoordinator {
    feeds: Vec<FeedState>,
    queue_tx: mpsc::Sender<QueueCommand>,
    http_client: reqwest::Client,
    feed_history: FeedHistoryDb,
}

struct FeedState {
    config: FeedConfig,
    filter: FeedFilter,
    last_fetch: Option<Instant>,
    info: FeedInfo,
}

impl FeedCoordinator {
    /// Main loop — runs forever, polling feeds on their intervals.
    pub async fn run(mut self, mut shutdown: broadcast::Receiver<()>) {
        loop {
            let next_due = self.next_due_feed();
            tokio::select! {
                _ = tokio::time::sleep_until(next_due) => {
                    self.poll_due_feeds().await;
                }
                _ = shutdown.recv() => break,
            }
        }
    }

    async fn poll_due_feeds(&mut self) {
        for feed in &mut self.feeds {
            if feed.is_due() {
                match self.fetch_and_process(feed).await {
                    Ok(added) => {
                        tracing::info!(feed = %feed.config.name, added, "feed poll complete");
                    }
                    Err(e) => {
                        tracing::warn!(feed = %feed.config.name, err = %e, "feed poll failed");
                    }
                }
                feed.last_fetch = Some(Instant::now());
            }
        }
    }

    async fn fetch_and_process(&self, feed: &mut FeedState) -> Result<u32> {
        let body = self.http_client.get(&feed.config.url).send().await?.text().await?;
        let items = parse_feed(&body)?;
        let mut added = 0u32;

        for item in items {
            if self.feed_history.already_processed(&feed.config, &item) {
                continue;
            }
            let result = feed.filter.evaluate(&item);
            match result.action {
                FilterAction::Accept => {
                    self.queue_tx
                        .send(QueueCommand::AddUrl {
                            url: item.url.clone(),
                            category: result.category.unwrap_or(feed.config.category.clone()),
                            priority: result.priority.unwrap_or(feed.config.priority),
                            paused: result.pause.unwrap_or(feed.config.pause_nzb),
                        })
                        .await?;
                    self.feed_history.mark_fetched(&feed.config, &item);
                    added += 1;
                }
                FilterAction::Reject => {
                    self.feed_history.mark_skipped(&feed.config, &item);
                }
                _ => {}
            }
        }
        Ok(added)
    }
}
```

## Feed History

Processed feed items are tracked in a history database to prevent re-downloads across restarts.

| Field | Description |
|-------|-------------|
| `url` | The NZB URL (primary key within a feed) |
| `status` | `Fetched` or `Skipped` |
| `first_seen` | Timestamp of first occurrence |
| `last_seen` | Timestamp of most recent occurrence |

The `FeedHistoryDays` option controls how long entries are retained. Items older than this are purged on each feed poll, allowing previously-rejected items to be reconsidered if filter rules change.

```rust
pub struct FeedHistoryDb {
    entries: HashMap<(u32, String), FeedHistoryEntry>, // (feed_id, url)
    retention_days: u32,
}

pub struct FeedHistoryEntry {
    pub status: FeedItemStatus,
    pub first_seen: chrono::DateTime<chrono::Utc>,
    pub last_seen: chrono::DateTime<chrono::Utc>,
}

impl FeedHistoryDb {
    pub fn already_processed(&self, feed: &FeedConfig, item: &FeedItemInfo) -> bool {
        self.entries.contains_key(&(feed.id, item.url.clone()))
    }

    pub fn purge_old(&mut self, now: chrono::DateTime<chrono::Utc>) {
        let cutoff = now - chrono::Duration::days(self.retention_days as i64);
        self.entries.retain(|_, entry| entry.last_seen > cutoff);
    }
}
```

## FeedInfo

Aggregate status exposed via the API for each feed:

```rust
pub struct FeedInfo {
    pub id: u32,
    pub name: String,
    pub url: String,
    pub last_update: Option<chrono::DateTime<chrono::Utc>>,
    pub next_update: Option<chrono::DateTime<chrono::Utc>>,
    pub status: FeedStatus,
    pub item_count: u32,
    pub error: Option<String>,
}

pub enum FeedStatus {
    Idle,
    Fetching,
    Error,
}
```
